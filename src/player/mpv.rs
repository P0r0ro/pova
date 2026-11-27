//! Headless mpv controlled over its JSON IPC socket.
//!
//! The actor owns the mpv child process. It accepts [`PlayerCmd`]s and emits
//! [`PlayerEvent`]s. mpv's internal playlist only ever holds the current track
//! plus the preloaded next one (for gapless playback); the real queue lives in
//! the app.

use std::collections::HashSet;
use std::path::PathBuf;
use std::process::Stdio;
use std::time::Duration;

use anyhow::Context;
use serde_json::{Value, json};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::UnixStream;
use tokio::process::{Child, Command};
use tokio::sync::mpsc::{UnboundedReceiver, UnboundedSender};

#[derive(Debug, Clone)]
pub enum PlayerCmd {
    /// Replace playback with `url`, preloading `next` after it.
    Load {
        url: String,
        next: Option<String>,
        /// Start this many seconds in (resume).
        start: Option<f64>,
        /// Load without starting playback (resume).
        paused: bool,
    },
    /// Replace whatever is queued after the current file.
    SetNext(Option<String>),
    TogglePause,
    SeekRelative(f64),
    SeekAbsolute(f64),
    SetVolume(f64),
    SetMute(bool),
    /// Loop the current file forever (repeat-one).
    SetLoopFile(bool),
    /// Playback speed (mpv keeps the pitch).
    SetSpeed(f64),
    /// Crossfade gain, 0 to 1, through mpv's `volume-gain` (mpv 0.38+): a
    /// separate stage, so the user's volume is untouched, and no filter.
    FadeGain(f64),
    /// Skip to the preloaded next song (overlap crossfade).
    PlaylistNext,
    /// Even out loudness between songs (adds / removes [`NORMALIZE_FILTER`]).
    SetNormalize(bool),
    Stop,
    Quit,
}

#[derive(Debug, Clone)]
pub enum PlayerEvent {
    Ready,
    TimePos(f64),
    Duration(f64),
    Paused(bool),
    Volume(f64),
    Muted(bool),
    /// The file mpv is currently playing changed.
    Path(Option<String>),
    /// mpv has nothing to play.
    Idle(bool),
    Buffering(bool),
    /// Playback (re)started after a seek or at a new file.
    Restarted,
    /// This mpv has no `volume-gain` (older than 0.38): no smooth fades.
    FadeUnsupported,
    /// A file finished; `reason` is mpv's end-file reason (eof, error, ...).
    EndFile {
        reason: String,
        error: Option<String>,
    },
    Error(String),
    Exited,
}

pub struct MpvConfig {
    pub socket: PathBuf,
    pub volume: u8,
    pub extra_args: Vec<String>,
    /// `--audio-client-name`, unique per pova instance (see [`audio_client_name`]).
    pub client_name: String,
    /// Start with [`NORMALIZE_FILTER`] on.
    pub normalize: bool,
    /// pova's own media controls are on: keep mpv from loading the
    /// mpv-mpris script, or desktops would show two players.
    pub skip_mpv_mpris: bool,
}

/// Label of pova's loudness filter in mpv's `af` chain, so it can be removed
/// again without touching filters from the user's `mpv_args`.
const NORMALIZE_LABEL: &str = "@povanorm";

/// Request id of crossfade gain changes: older mpv (before 0.38) has no
/// `volume-gain`, and those failures shouldn't fill the log.
const FADE_REQUEST: u64 = 0xFADE_0000_0000;

/// EBU R128 loudness normalization to -14 LUFS (YouTube's own reference
/// level), with a true-peak ceiling so loud masters don't clip. loudnorm
/// outputs 192 kHz in this mode, so resample back to a normal rate.
pub const NORMALIZE_FILTER: &str = "lavfi=[loudnorm=I=-14:TP=-1.5:LRA=11,aresample=48000]";

pub(crate) fn normalize_filter() -> String {
    format!("{NORMALIZE_LABEL}:{NORMALIZE_FILTER}")
}

const OBSERVED: &[&str] = &[
    "time-pos",
    "duration",
    "pause",
    "volume",
    "mute",
    "path",
    "idle-active",
    "paused-for-cache",
];

fn spawn_mpv(cfg: &MpvConfig) -> anyhow::Result<Child> {
    let _ = std::fs::remove_file(&cfg.socket);
    let mut cmd = Command::new("mpv");
    cmd.arg("--idle=yes")
        .arg("--no-video")
        .arg("--no-terminal")
        .arg("--force-window=no")
        .arg("--audio-display=no")
        .arg(format!("--input-ipc-server={}", cfg.socket.display()))
        .arg(format!("--audio-client-name={}", cfg.client_name))
        .arg(format!("--volume={}", cfg.volume))
        .arg("--volume-max=100")
        .arg("--prefetch-playlist=yes")
        .arg("--gapless-audio=weak")
        .arg("--cache=yes")
        .arg("--ytdl-format=bestaudio/best")
        .arg("--keep-open=no");
    if cfg.normalize {
        cmd.arg(format!("--af-append={}", normalize_filter()));
    }
    if cfg.skip_mpv_mpris
        && let Some(scripts) = scripts_for_pova(&cfg.extra_args)
    {
        // Load every other script by hand, so the user's own still run.
        cmd.arg("--load-scripts=no");
        for s in scripts {
            cmd.arg(format!("--scripts-append={}", s.display()));
        }
    }
    // yt-dlp needs a JS runtime for YouTube's signature challenges; it only
    // enables deno by default, so point it at node when that is all we have.
    if which("deno").is_none() && which("node").is_some() {
        cmd.arg("--ytdl-raw-options-append=js-runtimes=node");
    }
    cmd.args(&cfg.extra_args)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .kill_on_drop(true);
    // SAFETY: prctl is async-signal-safe; ensures mpv dies with us even on SIGKILL.
    unsafe {
        cmd.pre_exec(|| {
            libc::prctl(libc::PR_SET_PDEATHSIG, libc::SIGTERM);
            Ok(())
        });
    }
    cmd.spawn().context("failed to start mpv (is it installed?)")
}

/// PipeWire client name of this instance's mpv: `pova-<pid>`. Lets the
/// visualizer find *our* stream even when several pova sessions run.
pub fn audio_client_name() -> String {
    format!("pova-{}", std::process::id())
}

fn is_executable(p: &std::path::Path) -> bool {
    use std::os::unix::fs::PermissionsExt;
    p.is_file() && p.metadata().is_ok_and(|m| m.permissions().mode() & 0o111 != 0)
}

/// mpv's config folders, highest priority first, as mpv searches them:
/// `$MPV_HOME` (or `~/.config/mpv`, then the old `~/.mpv`), then
/// `$XDG_CONFIG_DIRS` (default `/etc/xdg`), then `/etc/mpv`.
fn mpv_config_dirs() -> Vec<PathBuf> {
    let env = |k| std::env::var_os(k).filter(|v| !v.is_empty()).map(PathBuf::from);
    let mut dirs = Vec::new();
    if let Some(home) = env("MPV_HOME") {
        dirs.push(home);
    } else {
        let home = env("HOME");
        if let Some(c) = env("XDG_CONFIG_HOME").or_else(|| home.as_ref().map(|h| h.join(".config"))) {
            dirs.push(c.join("mpv"));
        }
        if let Some(h) = home {
            dirs.push(h.join(".mpv"));
        }
    }
    let xdg = std::env::var("XDG_CONFIG_DIRS").ok().filter(|v| !v.is_empty());
    for d in xdg
        .as_deref()
        .unwrap_or("/etc/xdg")
        .split(':')
        .filter(|d| !d.is_empty())
    {
        dirs.push(PathBuf::from(d).join("mpv"));
    }
    dirs.push(PathBuf::from("/etc/mpv"));
    dirs
}

/// The user already decides which scripts load (in `mpv_args` or an
/// mpv.conf): then pova leaves script loading alone.
fn user_controls_scripts(extra_args: &[String], config_dirs: &[PathBuf]) -> bool {
    let sets = |line: &str| {
        let l = line.trim().trim_start_matches("--");
        [
            "load-scripts",
            "scripts",
            "script=",
            "script ",
            "config-dir",
            "no-config",
            "config=",
        ]
        .iter()
        .any(|k| l.starts_with(k))
    };
    extra_args.iter().any(|a| sets(a))
        || config_dirs
            .iter()
            .any(|d| std::fs::read_to_string(d.join("mpv.conf")).is_ok_and(|conf| conf.lines().any(sets)))
}

/// Whether mpv-mpris is installed where mpv would load it.
pub fn has_mpv_mpris() -> bool {
    let dirs: Vec<PathBuf> = mpv_config_dirs().iter().map(|d| d.join("scripts")).collect();
    scripts_without_mpris(&dirs).is_some()
}

/// The scripts to load instead of mpv's own autoloading, so mpv-mpris stays
/// out of pova's mpv. None (change nothing) when mpv-mpris isn't installed
/// or the user manages scripts themselves.
fn scripts_for_pova(extra_args: &[String]) -> Option<Vec<PathBuf>> {
    let config = mpv_config_dirs();
    let scripts: Vec<PathBuf> = config.iter().map(|d| d.join("scripts")).collect();
    let keep = scripts_without_mpris(&scripts)?;
    if user_controls_scripts(extra_args, &config) {
        tracing::info!("mpv-mpris found, but your mpv settings choose the scripts; leaving them alone");
        return None;
    }
    Some(keep)
}

/// When the mpv-mpris script (`mpris.so`, or a script or folder named
/// `mpris`) is among the scripts mpv would load from `dirs`, the others to
/// load instead. None when there's no mpv-mpris, so nothing changes. A name
/// in an earlier folder hides the same name in later ones, like mpv does.
fn scripts_without_mpris(dirs: &[PathBuf]) -> Option<Vec<PathBuf>> {
    let mut seen = HashSet::new();
    let mut keep = Vec::new();
    let mut found = false;
    for dir in dirs {
        let Ok(entries) = std::fs::read_dir(dir) else { continue };
        let mut entries: Vec<_> = entries.flatten().map(|e| e.path()).collect();
        entries.sort();
        for path in entries {
            let Some(name) = path.file_name().and_then(|n| n.to_str()).map(str::to_string) else {
                continue;
            };
            // mpv skips hidden files and editor backups too.
            if name.starts_with('.') || name.ends_with('~') || !seen.insert(name.clone()) {
                continue;
            }
            let stem = name.split('.').next().unwrap_or_default();
            if stem == "mpris" {
                found = true;
            } else {
                keep.push(path);
            }
        }
    }
    found.then_some(keep)
}

pub fn which(bin: &str) -> Option<PathBuf> {
    let path = std::env::var_os("PATH")?;
    std::env::split_paths(&path)
        .map(|d| d.join(bin))
        .find(|p| is_executable(p))
}

/// File named `bin` on PATH that exists but lacks execute permission (`chmod +x`).
pub fn find_non_executable(bin: &str) -> Option<PathBuf> {
    let path = std::env::var_os("PATH")?;
    std::env::split_paths(&path)
        .map(|d| d.join(bin))
        .find(|p| p.is_file() && !is_executable(p))
}

/// Run the player actor until `Quit` or mpv exits.
pub async fn run(cfg: MpvConfig, mut cmds: UnboundedReceiver<PlayerCmd>, events: UnboundedSender<PlayerEvent>) {
    if let Err(e) = run_inner(&cfg, &mut cmds, &events).await {
        tracing::error!("player: {e:#}");
        let _ = events.send(PlayerEvent::Error(format!("{e:#}")));
    }
    let _ = std::fs::remove_file(&cfg.socket);
    let _ = events.send(PlayerEvent::Exited);
}

async fn run_inner(
    cfg: &MpvConfig,
    cmds: &mut UnboundedReceiver<PlayerCmd>,
    events: &UnboundedSender<PlayerEvent>,
) -> anyhow::Result<()> {
    let mut child = spawn_mpv(cfg)?;

    // Wait for the IPC socket to appear.
    let mut stream = None;
    for _ in 0..100 {
        if let Ok(s) = UnixStream::connect(&cfg.socket).await {
            stream = Some(s);
            break;
        }
        if let Some(status) = child.try_wait()? {
            anyhow::bail!("mpv exited early ({status})");
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    let stream = stream.context("timed out connecting to mpv IPC socket")?;
    let (read, mut write) = stream.into_split();

    let mut request_id: u64 = 0;
    let mut send = async |write: &mut tokio::net::unix::OwnedWriteHalf, cmd: Value| -> anyhow::Result<()> {
        request_id += 1;
        let mut line = serde_json::to_vec(&json!({ "command": cmd, "request_id": request_id }))?;
        line.push(b'\n');
        write.write_all(&line).await?;
        Ok(())
    };

    for (i, name) in OBSERVED.iter().enumerate() {
        send(&mut write, json!(["observe_property", i + 1, name])).await?;
    }
    let _ = events.send(PlayerEvent::Ready);

    let mut lines = BufReader::new(read).lines();
    loop {
        tokio::select! {
            line = lines.next_line() => {
                match line? {
                    Some(line) => handle_line(&line, events),
                    None => break, // socket closed: mpv quit
                }
            }
            cmd = cmds.recv() => {
                let Some(cmd) = cmd else { break };
                tracing::debug!("mpv <- {cmd:?}");
                match cmd {
                    PlayerCmd::Load { url, next, start, paused } => {
                        if let Some(s) = start {
                            // Per-file options (mpv >= 0.38: index, then options).
                            send(&mut write, json!(["set_property", "pause", paused])).await?;
                            send(&mut write, json!(["loadfile", url, "replace", -1, format!("start={s:.1}")])).await?;
                        } else if paused {
                            // Plain 3-arg loadfile works on older mpv (< 0.38) too.
                            send(&mut write, json!(["set_property", "pause", true])).await?;
                            send(&mut write, json!(["loadfile", url, "replace"])).await?;
                        } else {
                            send(&mut write, json!(["loadfile", url, "replace"])).await?;
                            send(&mut write, json!(["set_property", "pause", false])).await?;
                        }
                        if let Some(next) = next {
                            send(&mut write, json!(["loadfile", next, "append"])).await?;
                        }
                    }
                    PlayerCmd::SetNext(next) => {
                        // playlist-clear keeps the currently playing entry.
                        send(&mut write, json!(["playlist-clear"])).await?;
                        if let Some(next) = next {
                            send(&mut write, json!(["loadfile", next, "append"])).await?;
                        }
                    }
                    PlayerCmd::TogglePause => send(&mut write, json!(["cycle", "pause"])).await?,
                    PlayerCmd::SeekRelative(s) => send(&mut write, json!(["seek", s, "relative"])).await?,
                    PlayerCmd::SeekAbsolute(s) => send(&mut write, json!(["seek", s, "absolute"])).await?,
                    PlayerCmd::SetVolume(v) => send(&mut write, json!(["set_property", "volume", v])).await?,
                    PlayerCmd::SetMute(m) => send(&mut write, json!(["set_property", "mute", m])).await?,
                    PlayerCmd::SetLoopFile(on) => {
                        send(&mut write, json!(["set_property", "loop-file", if on { "inf" } else { "no" }])).await?
                    }
                    PlayerCmd::SetNormalize(on) => {
                        if on {
                            send(&mut write, json!(["af", "add", normalize_filter()])).await?
                        } else {
                            send(&mut write, json!(["af", "remove", NORMALIZE_LABEL])).await?
                        }
                    }
                    PlayerCmd::SetSpeed(s) => send(&mut write, json!(["set_property", "speed", s])).await?,
                    PlayerCmd::FadeGain(g) => {
                        let db = if g <= 0.001 { -96.0 } else { (20.0 * g.min(1.0).log10()).max(-96.0) };
                        let line = json!({ "command": ["set_property", "volume-gain", db], "request_id": FADE_REQUEST });
                        let mut line = serde_json::to_vec(&line)?;
                        line.push(b'\n');
                        write.write_all(&line).await?;
                    }
                    PlayerCmd::PlaylistNext => send(&mut write, json!(["playlist-next", "weak"])).await?,
                    PlayerCmd::Stop => send(&mut write, json!(["stop"])).await?,
                    PlayerCmd::Quit => {
                        let _ = send(&mut write, json!(["quit"])).await;
                        break;
                    }
                }
            }
            status = child.wait() => {
                anyhow::bail!("mpv exited unexpectedly ({})", status?);
            }
        }
    }

    // Graceful shutdown, then make sure.
    if tokio::time::timeout(Duration::from_millis(800), child.wait())
        .await
        .is_err()
    {
        let _ = child.kill().await;
    }
    Ok(())
}

fn handle_line(line: &str, events: &UnboundedSender<PlayerEvent>) {
    let Ok(v) = serde_json::from_str::<Value>(line) else {
        tracing::warn!("mpv: unparsable line {line}");
        return;
    };
    if let Some(err) = v.get("error").and_then(Value::as_str)
        && err != "success"
    {
        if v.get("request_id").and_then(Value::as_u64) == Some(FADE_REQUEST) {
            tracing::debug!("crossfade gain needs mpv 0.38 or newer: {line}");
            let _ = events.send(PlayerEvent::FadeUnsupported);
        } else {
            tracing::warn!("mpv command failed: {line}");
        }
    }
    let Some(event) = v.get("event").and_then(Value::as_str) else {
        return;
    };
    let ev = match event {
        "property-change" => {
            let name = v.get("name").and_then(Value::as_str).unwrap_or_default();
            let data = v.get("data");
            let num = || data.and_then(Value::as_f64);
            let flag = || data.and_then(Value::as_bool);
            match name {
                "time-pos" => num().map(PlayerEvent::TimePos),
                "duration" => num().map(PlayerEvent::Duration),
                "pause" => flag().map(PlayerEvent::Paused),
                "volume" => num().map(PlayerEvent::Volume),
                "mute" => flag().map(PlayerEvent::Muted),
                "path" => Some(PlayerEvent::Path(data.and_then(Value::as_str).map(str::to_string))),
                "idle-active" => flag().map(PlayerEvent::Idle),
                "paused-for-cache" => flag().map(PlayerEvent::Buffering),
                _ => None,
            }
        }
        "playback-restart" => Some(PlayerEvent::Restarted),
        "end-file" => Some(PlayerEvent::EndFile {
            reason: v.get("reason").and_then(Value::as_str).unwrap_or("unknown").to_string(),
            error: v.get("file_error").and_then(Value::as_str).map(str::to_string),
        }),
        _ => None,
    };
    if let Some(ev) = ev {
        let _ = events.send(ev);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mpv_mpris_is_left_out_and_other_scripts_kept() {
        // Under target/, never /tmp (a RAM disk on the maintainer's machine).
        let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("target/test-mpv-scripts");
        let _ = std::fs::remove_dir_all(&root);
        let (user, system) = (root.join("user"), root.join("system"));
        for (dir, names) in [
            (&user, &["skip.lua", ".hidden.lua", "mine"][..]),
            (&system, &["mpris.so", "skip.lua", "other.js"][..]),
        ] {
            std::fs::create_dir_all(dir).unwrap();
            for n in names {
                std::fs::write(dir.join(n), "").unwrap();
            }
        }
        let got = scripts_without_mpris(&[user.clone(), system.clone(), root.join("missing")]).unwrap();
        // The user's skip.lua hides the system one; hidden files are skipped.
        assert_eq!(
            got,
            vec![user.join("mine"), user.join("skip.lua"), system.join("other.js")]
        );
        // The user's own script settings win.
        assert!(!user_controls_scripts(&[], std::slice::from_ref(&user)));
        std::fs::write(user.join("mpv.conf"), "volume=50\nload-scripts=no\n").unwrap();
        assert!(user_controls_scripts(&[], std::slice::from_ref(&user)));
        assert!(user_controls_scripts(&["--scripts=/x.lua".into()], &[]));
        assert!(!user_controls_scripts(&["--volume=50".into()], &[]));
        // Without mpv-mpris nothing changes.
        std::fs::remove_file(system.join("mpris.so")).unwrap();
        assert_eq!(scripts_without_mpris(&[user, system]), None);
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn parses_property_events() {
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
        handle_line(
            r#"{"event":"property-change","id":1,"name":"time-pos","data":12.5}"#,
            &tx,
        );
        handle_line(r#"{"event":"property-change","id":5,"name":"path","data":null}"#, &tx);
        handle_line(
            r#"{"event":"end-file","reason":"error","file_error":"loading failed"}"#,
            &tx,
        );
        handle_line(r#"{"request_id":1,"error":"success"}"#, &tx);
        assert!(matches!(rx.try_recv(), Ok(PlayerEvent::TimePos(t)) if t == 12.5));
        assert!(matches!(rx.try_recv(), Ok(PlayerEvent::Path(None))));
        assert!(matches!(rx.try_recv(), Ok(PlayerEvent::EndFile { reason, .. }) if reason == "error"));
        assert!(rx.try_recv().is_err());
    }
}

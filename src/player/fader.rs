//! The overlap crossfade (`crossfade = "overlap"`): a second, short-lived mpv
//! that plays the last seconds of the outgoing song and fades it out, while
//! the main mpv has already moved on to the next song and fades that in.
//!
//! It exists only around a song change (about ten seconds), then exits. The
//! main mpv stays the one source of truth for the queue, time, lyrics,
//! visualizer and media controls; it only ever gets a normal "next".
//!
//! Flow: `Prepare` starts the helper paused at the overlap point, from the
//! song's pre-resolved direct URL. Once it's loaded it reports `Ready`. `Go`
//! unpauses it and ramps its own volume down to 0, then it quits. `Cancel`
//! (a seek, a skip, a pause) kills it at once.

use std::path::PathBuf;
use std::process::Stdio;
use std::time::Duration;

use serde_json::{Value, json};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::UnixStream;
use tokio::process::{Child, Command};
use tokio::sync::mpsc::{UnboundedReceiver, UnboundedSender};

#[derive(Debug, Clone)]
pub struct Prepare {
    /// Matches this helper's events to the request (older ones are ignored).
    pub id: u64,
    /// Direct audio URL of the outgoing song.
    pub url: String,
    /// Where the overlap starts, in seconds.
    pub start: f64,
    pub volume: f64,
    pub speed: f64,
    /// pova's loudness filter, if it's on (so both songs sound alike).
    pub filter: Option<String>,
    /// Audio output options from the user's `mpv_args` (`--ao`, `--audio-device`).
    pub audio_args: Vec<String>,
    /// How long the outgoing song fades out.
    pub fade: f64,
}

#[derive(Debug)]
pub enum FaderCmd {
    Prepare(Prepare),
    Go,
    Cancel,
}

#[derive(Debug, Clone, PartialEq)]
pub enum FaderEvent {
    /// Loaded, paused at the overlap point: say `Go`.
    Ready(u64),
    /// Couldn't start in time; this song change falls back to the smooth fade.
    Failed(u64),
}

/// How long a helper may take to load before it's given up on.
const LOAD_TIMEOUT: Duration = Duration::from_secs(6);

pub async fn run(runtime_dir: PathBuf, mut cmds: UnboundedReceiver<FaderCmd>, events: UnboundedSender<FaderEvent>) {
    let socket = runtime_dir.join(format!("pova-fade-{}.sock", std::process::id()));
    let mut pending: Option<FaderCmd> = None;
    loop {
        let cmd = match pending.take() {
            Some(c) => c,
            None => match cmds.recv().await {
                Some(c) => c,
                None => break,
            },
        };
        let FaderCmd::Prepare(p) = cmd else { continue };
        match fade_once(&socket, &p, &mut cmds, &events).await {
            Ok(next) => pending = next,
            Err(e) => {
                tracing::info!("crossfade helper: {e:#}");
                let _ = events.send(FaderEvent::Failed(p.id));
            }
        }
        let _ = std::fs::remove_file(&socket);
    }
}

/// Run one helper from Prepare to the end of its fade. Returns a Prepare that
/// arrived meanwhile, to start next.
async fn fade_once(
    socket: &std::path::Path,
    p: &Prepare,
    cmds: &mut UnboundedReceiver<FaderCmd>,
    events: &UnboundedSender<FaderEvent>,
) -> anyhow::Result<Option<FaderCmd>> {
    let _ = std::fs::remove_file(socket);
    // Dropping `child` kills the helper (kill_on_drop), on every early return.
    let mut child = spawn(socket, p)?;
    let mut stream = None;
    for _ in 0..60 {
        if let Ok(s) = UnixStream::connect(socket).await {
            stream = Some(s);
            break;
        }
        if child.try_wait()?.is_some() {
            anyhow::bail!("mpv exited early");
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    let (read, mut write) = stream.ok_or_else(|| anyhow::anyhow!("no IPC socket"))?.into_split();
    let mut lines = BufReader::new(read).lines();

    // Wait until it's loaded and paused at the start point, or a cancel.
    let ready = tokio::time::timeout(LOAD_TIMEOUT, async {
        loop {
            tokio::select! {
                line = lines.next_line() => match line? {
                    Some(l) if is_event(&l, "playback-restart") => return Ok(None),
                    Some(l) if is_event(&l, "end-file") => anyhow::bail!("couldn't load the stream"),
                    Some(_) => {}
                    None => anyhow::bail!("mpv closed the socket"),
                },
                cmd = cmds.recv() => match cmd {
                    Some(FaderCmd::Go) => {}
                    Some(c) => return Ok(Some(c)),
                    None => return Ok(Some(FaderCmd::Cancel)),
                },
            }
        }
    })
    .await
    .map_err(|_| anyhow::anyhow!("took too long to load"))??;
    if let Some(c) = ready {
        // Cancelled (or a new Prepare) before it was ready.
        return Ok(next_prepare(c));
    }
    let _ = events.send(FaderEvent::Ready(p.id));

    // Wait for Go.
    match cmds.recv().await {
        Some(FaderCmd::Go) => {}
        Some(c) => return Ok(next_prepare(c)),
        None => return Ok(None),
    }
    // From here on errors aren't failures: the helper quits by itself when
    // the song's stream ends, sometimes a moment before the fade does.
    if send(&mut write, json!(["set_property", "pause", false])).await.is_err() {
        return Ok(None);
    }
    // Fade its own volume to 0 (not the user's: that's the main mpv's).
    let steps = (p.fade * 20.0).round().max(1.0) as u32;
    let tick = Duration::from_secs_f64(p.fade / steps as f64);
    for i in 1..=steps {
        tokio::select! {
            _ = tokio::time::sleep(tick) => {}
            cmd = cmds.recv() => match cmd {
                Some(FaderCmd::Go) => {}
                Some(c) => return Ok(next_prepare(c)),
                None => return Ok(None),
            },
        }
        // Equal-power fade (loudness gain √(1-x), matching the main mpv's
        // √x fade-in, so the overlap has no dip). mpv's volume property is
        // cubic in loudness, hence the cube root.
        let left = 1.0 - i as f64 / steps as f64;
        let v = p.volume * left.sqrt().cbrt();
        if send(&mut write, json!(["set_property", "volume", v])).await.is_err() {
            return Ok(None);
        }
    }
    let _ = send(&mut write, json!(["quit"])).await;
    let _ = tokio::time::timeout(Duration::from_millis(500), child.wait()).await;
    Ok(None)
}

fn next_prepare(c: FaderCmd) -> Option<FaderCmd> {
    matches!(c, FaderCmd::Prepare(_)).then_some(c)
}

fn is_event(line: &str, name: &str) -> bool {
    serde_json::from_str::<Value>(line)
        .ok()
        .and_then(|v| v.get("event").and_then(Value::as_str).map(|e| e == name))
        .unwrap_or(false)
}

async fn send(write: &mut tokio::net::unix::OwnedWriteHalf, cmd: Value) -> anyhow::Result<()> {
    let mut line = serde_json::to_vec(&json!({ "command": cmd }))?;
    line.push(b'\n');
    write.write_all(&line).await?;
    Ok(())
}

/// The helper's command line. `--no-config`: none of the user's mpv scripts
/// (mpv-mpris would show a second player for a few seconds) or settings
/// like saving the position; only their audio output options are kept.
fn args(socket: &std::path::Path, p: &Prepare) -> Vec<String> {
    let mut a = vec![
        "--no-config".to_string(),
        "--idle=no".into(),
        "--no-video".into(),
        "--no-terminal".into(),
        "--force-window=no".into(),
        "--audio-display=no".into(),
        "--ytdl=no".into(),
        "--pause".into(),
        format!("--start={:.2}", p.start),
        format!("--volume={:.1}", p.volume),
        "--volume-max=100".into(),
        format!("--speed={}", p.speed),
        "--audio-client-name=pova-crossfade".into(),
        format!("--input-ipc-server={}", socket.display()),
    ];
    if let Some(f) = &p.filter {
        a.push(format!("--af-append={f}"));
    }
    a.extend(p.audio_args.iter().cloned());
    // `--` so the URL is never read as an option.
    a.push("--".into());
    a.push(p.url.clone());
    a
}

/// The audio output options in the user's `mpv_args`, for the helper.
pub fn audio_args(extra_args: &[String]) -> Vec<String> {
    extra_args
        .iter()
        .filter(|a| {
            let a = a.trim_start_matches("--");
            a.starts_with("ao=") || a.starts_with("audio-device=") || a.starts_with("audio-exclusive")
        })
        .cloned()
        .collect()
}

fn spawn(socket: &std::path::Path, p: &Prepare) -> anyhow::Result<Child> {
    let mut cmd = Command::new("mpv");
    cmd.args(args(socket, p))
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .kill_on_drop(true);
    // SAFETY: prctl is async-signal-safe; the helper dies with pova even on SIGKILL.
    unsafe {
        cmd.pre_exec(|| {
            libc::prctl(libc::PR_SET_PDEATHSIG, libc::SIGTERM);
            Ok(())
        });
    }
    Ok(cmd.spawn()?)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn helper_runs_without_the_users_scripts_and_keeps_their_audio_device() {
        let p = Prepare {
            id: 1,
            url: "-https://x".into(),
            start: 200.0,
            volume: 70.0,
            speed: 1.0,
            filter: Some("@povanorm:lavfi=[loudnorm]".into()),
            audio_args: audio_args(&["--audio-device=pipewire/hdmi".into(), "--volume=30".into()]),
            fade: 6.0,
        };
        let a = args(std::path::Path::new("/r/f.sock"), &p);
        assert_eq!(a[0], "--no-config");
        assert!(a.contains(&"--start=200.00".to_string()) && a.contains(&"--pause".to_string()));
        assert!(a.contains(&"--audio-device=pipewire/hdmi".to_string()));
        assert!(!a.iter().any(|x| x == "--volume=30"));
        // The URL comes last, after "--".
        assert_eq!(&a[a.len() - 2..], ["--", "-https://x"]);
    }
}

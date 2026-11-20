//! Headless mpv controlled over its JSON IPC socket.
//!
//! The actor owns the mpv child process. It accepts [`PlayerCmd`]s and emits
//! [`PlayerEvent`]s. mpv's internal playlist only ever holds the current track
//! plus the preloaded next one (for gapless playback); the real queue lives in
//! the app.

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
    /// Receives mpv's PID so the visualizer can find its PipeWire stream.
    pub pid_out: std::sync::Arc<std::sync::atomic::AtomicU32>,
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
        .arg("--audio-client-name=pova")
        .arg(format!("--volume={}", cfg.volume))
        .arg("--volume-max=100")
        .arg("--prefetch-playlist=yes")
        .arg("--gapless-audio=weak")
        .arg("--cache=yes")
        .arg("--ytdl-format=bestaudio/best")
        .arg("--keep-open=no");
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

pub fn which(bin: &str) -> Option<PathBuf> {
    let path = std::env::var_os("PATH")?;
    std::env::split_paths(&path).map(|d| d.join(bin)).find(|p| p.is_file())
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
    if let Some(pid) = child.id() {
        cfg.pid_out.store(pid, std::sync::atomic::Ordering::Relaxed);
    }

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
                    PlayerCmd::Load { url, next } => {
                        send(&mut write, json!(["loadfile", url, "replace"])).await?;
                        send(&mut write, json!(["set_property", "pause", false])).await?;
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
        tracing::warn!("mpv command failed: {line}");
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

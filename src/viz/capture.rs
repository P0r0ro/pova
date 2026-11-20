//! Audio capture: finds mpv's PipeWire output node and records it with
//! `pw-record` (no PipeWire dev headers needed), pushing f32 samples into a
//! lock-free SPSC ring buffer.

use std::io::Read;
use std::process::{Child, Command, Stdio};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::thread;
use std::time::Duration;

use std::os::unix::process::CommandExt;

use serde_json::Value;

use crate::config::VizSource;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Target {
    /// object.serial of mpv's Stream/Output/Audio node.
    Node(u64),
    /// Monitor of the default sink.
    DefaultSink,
}

pub struct CaptureConfig {
    pub source: VizSource,
    pub rate: u32,
    /// PID of our mpv, used to pick *our* stream when several exist (0 = unknown).
    pub mpv_pid: Arc<AtomicU32>,
}

/// Look up the serial of mpv's playback stream in the PipeWire graph.
fn find_mpv_node(pid: u32) -> Option<u64> {
    let out = Command::new("pw-dump").stderr(Stdio::null()).output().ok()?;
    let objects: Vec<Value> = serde_json::from_slice(&out.stdout).ok()?;
    // Nodes without a process id are accepted only if nothing matches exactly;
    // nodes owned by another pova instance are never picked.
    let mut fallback = None;
    for obj in &objects {
        let Some(props) = obj.pointer("/info/props") else {
            continue;
        };
        let s = |k: &str| props.get(k).and_then(Value::as_str);
        if s("media.class") != Some("Stream/Output/Audio") {
            continue;
        }
        if s("application.name") != Some("pova") && s("node.name") != Some("pova") {
            continue;
        }
        let Some(serial) = props.get("object.serial").and_then(Value::as_u64) else {
            continue;
        };
        let node_pid = props
            .get("application.process.id")
            .and_then(|v| v.as_u64().or_else(|| v.as_str().and_then(|s| s.parse().ok())));
        match node_pid {
            Some(p) if pid != 0 && p == pid as u64 => return Some(serial),
            Some(_) if pid != 0 => {}
            _ => fallback = fallback.or(Some(serial)),
        }
    }
    fallback
}

fn find_target(cfg: &CaptureConfig) -> Option<Target> {
    match cfg.source {
        VizSource::System => Some(Target::DefaultSink),
        VizSource::Mpv => find_mpv_node(cfg.mpv_pid.load(Ordering::Relaxed)).map(Target::Node),
    }
}

fn spawn_recorder(target: Target, rate: u32) -> std::io::Result<Child> {
    let mut cmd = Command::new("pw-record");
    cmd.args(["--raw", "--format", "f32", "--channels", "2", "--latency", "10ms"])
        .args(["--rate", &rate.to_string()])
        .args(["-P", "node.name=pova-visualizer"])
        .args(["-P", "media.role=Production"]);
    match target {
        Target::Node(serial) => {
            cmd.args(["--target", &serial.to_string()]);
        }
        Target::DefaultSink => {
            cmd.args(["-P", "stream.capture.sink=true"]);
        }
    }
    cmd.arg("-")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null());
    // SAFETY: prctl is async-signal-safe; recorder dies with pova.
    unsafe {
        cmd.pre_exec(|| {
            libc::prctl(libc::PR_SET_PDEATHSIG, libc::SIGTERM);
            Ok(())
        });
    }
    cmd.spawn()
}

fn read_into(mut stdout: std::process::ChildStdout, mut producer: rtrb::Producer<f32>) -> rtrb::Producer<f32> {
    let mut buf = [0u8; 4096];
    let mut carry: Vec<u8> = Vec::with_capacity(4);
    loop {
        let n = match stdout.read(&mut buf) {
            Ok(0) | Err(_) => break,
            Ok(n) => n,
        };
        carry.extend_from_slice(&buf[..n]);
        let whole = carry.len() / 4 * 4;
        let samples = whole / 4;
        let writable = producer.slots().min(samples);
        if writable > 0
            && let Ok(mut chunk) = producer.write_chunk_uninit(writable)
        {
            let (a, b) = chunk.as_mut_slices();
            for (i, slot) in a.iter_mut().chain(b.iter_mut()).enumerate() {
                let o = i * 4;
                slot.write(f32::from_ne_bytes([carry[o], carry[o + 1], carry[o + 2], carry[o + 3]]));
            }
            // SAFETY: every slot of the chunk was initialised above.
            unsafe { chunk.commit_all() };
        }
        // Samples that did not fit are dropped (UI is behind; stay real-time).
        carry.drain(..whole);
    }
    producer
}

/// Capture loop; runs on its own thread until `quit` is set.
pub fn run(cfg: CaptureConfig, producer: rtrb::Producer<f32>, quit: Arc<AtomicBool>) {
    let mut producer = Some(producer);
    while !quit.load(Ordering::Relaxed) {
        let Some(target) = find_target(&cfg) else {
            thread::sleep(Duration::from_millis(700));
            continue;
        };
        let mut child = match spawn_recorder(target, cfg.rate) {
            Ok(c) => c,
            Err(e) => {
                tracing::error!("pw-record failed to start: {e}");
                thread::sleep(Duration::from_secs(5));
                continue;
            }
        };
        tracing::info!("visualizer capturing {target:?}");
        let stdout = child.stdout.take().expect("piped stdout");
        let p = producer.take().expect("producer present");
        let reader = thread::spawn(move || read_into(stdout, p));

        let mut ticks = 0u32;
        loop {
            thread::sleep(Duration::from_millis(100));
            ticks += 1;
            if quit.load(Ordering::Relaxed) || reader.is_finished() {
                break;
            }
            // Re-check the graph every 3s: mpv recreates its stream when the
            // audio format changes or after going idle.
            if ticks.is_multiple_of(30) && find_target(&cfg) != Some(target) {
                break;
            }
        }
        let _ = child.kill();
        let _ = child.wait();
        producer = Some(reader.join().expect("reader thread"));
    }
}

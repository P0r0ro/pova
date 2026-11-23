//! Audio capture: finds mpv's PipeWire output node and records it with
//! `pw-record` (no PipeWire dev headers needed), pushing f32 samples into a
//! lock-free SPSC ring buffer.

use std::io::Read;
use std::process::{Child, Command, Stdio};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
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
    /// mpv's `--audio-client-name`, unique per pova instance (`pova-<pid>`).
    /// mpv's PipeWire stream carries no process id, so the name is the only
    /// reliable way to capture *this* instance's audio and not another's.
    pub client_name: String,
}

/// Look up the serial of mpv's playback stream in the PipeWire graph.
fn find_mpv_node(client_name: &str) -> Option<u64> {
    let out = Command::new("pw-dump").stderr(Stdio::null()).output().ok()?;
    let objects: Vec<Value> = serde_json::from_slice(&out.stdout).ok()?;
    pick_node(&objects, client_name)
}

/// The playback stream named exactly `client_name`.
fn pick_node(objects: &[Value], client_name: &str) -> Option<u64> {
    objects.iter().find_map(|obj| {
        let props = obj.pointer("/info/props")?;
        let s = |k: &str| props.get(k).and_then(Value::as_str);
        let ours = s("node.name") == Some(client_name) || s("application.name") == Some(client_name);
        if s("media.class") != Some("Stream/Output/Audio") || !ours {
            return None;
        }
        props.get("object.serial").and_then(Value::as_u64)
    })
}

fn find_target(cfg: &CaptureConfig) -> Option<Target> {
    match cfg.source {
        VizSource::System => Some(Target::DefaultSink),
        VizSource::Mpv => find_mpv_node(&cfg.client_name).map(Target::Node),
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

fn read_into(stdout: std::process::ChildStdout, producer: rtrb::Producer<f32>) -> rtrb::Producer<f32> {
    read_stream_into(stdout, producer)
}

fn read_stream_into<R: Read>(mut reader: R, mut producer: rtrb::Producer<f32>) -> rtrb::Producer<f32> {
    let mut buf = [0u8; 4096];
    let mut carry: Vec<u8> = Vec::with_capacity(8);
    loop {
        let n = match reader.read(&mut buf) {
            Ok(0) | Err(_) => break,
            Ok(n) => n,
        };
        carry.extend_from_slice(&buf[..n]);
        // Keep complete stereo frames (2 x f32 = 8 bytes) so L/R never swap.
        let whole = carry.len() / 8 * 8;
        let samples = whole / 4;
        let writable = (producer.slots() & !1).min(samples);
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn picks_only_this_instances_stream() {
        let graph: Vec<Value> = serde_json::from_str(
            r#"[
            {"info":{"props":{"media.class":"Stream/Output/Audio","node.name":"pova-111","object.serial":10}}},
            {"info":{"props":{"media.class":"Stream/Output/Audio","node.name":"pova-222","object.serial":20}}},
            {"info":{"props":{"media.class":"Stream/Input/Audio","node.name":"pova-222","object.serial":30}}},
            {"info":null}
        ]"#,
        )
        .unwrap();
        assert_eq!(pick_node(&graph, "pova-222"), Some(20));
        assert_eq!(pick_node(&graph, "pova-111"), Some(10));
        assert_eq!(pick_node(&graph, "pova-333"), None);
    }

    #[test]
    fn read_stream_preserves_stereo_alignment_across_odd_chunks() {
        struct OddChunks {
            data: Vec<u8>,
            pos: usize,
            sizes: &'static [usize],
            step: usize,
        }
        impl Read for OddChunks {
            fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
                if self.pos >= self.data.len() {
                    return Ok(0);
                }
                let want = self.sizes[self.step % self.sizes.len()];
                self.step += 1;
                let n = want.min(self.data.len() - self.pos).min(buf.len());
                buf[..n].copy_from_slice(&self.data[self.pos..self.pos + n]);
                self.pos += n;
                Ok(n)
            }
        }
        let samples = [1.0f32, -1.0, 2.0, -2.0, 3.0, -3.0];
        let mut bytes = Vec::new();
        for s in samples {
            bytes.extend_from_slice(&s.to_ne_bytes());
        }
        // Odd ring capacity (5 slots): must write at most 4 samples (2 stereo frames).
        let (producer, mut consumer) = rtrb::RingBuffer::<f32>::new(5);
        let reader = OddChunks {
            data: bytes,
            pos: 0,
            sizes: &[5, 3, 7, 1, 8],
            step: 0,
        };
        read_stream_into(reader, producer);
        let got: Vec<f32> = std::iter::from_fn(|| consumer.pop().ok()).collect();
        assert_eq!(got, vec![1.0, -1.0, 2.0, -2.0]);
    }
}

//! Desktop notifications when the song changes (`notifications = true`).
//!
//! Sent with `notify-send` (libnotify), so pova needs no D-Bus library. Each
//! new song replaces the previous popup instead of stacking them up, and the
//! cover art comes from the same disk cache the album-art actor fills.

use std::path::{Path, PathBuf};
use std::time::Duration;

use tokio::process::Command;
use tokio::sync::mpsc::UnboundedReceiver;

/// Shown when notifications are turned on without notify-send installed.
pub const MISSING: &str = "Notifications need the notify-send program. Install it \
     (Fedora: sudo dnf install libnotify · Debian, Ubuntu: sudo apt install libnotify-bin · \
     Arch: sudo pacman -S libnotify), then turn notifications on again in Esc → Configs.";

#[derive(Debug, Clone, PartialEq)]
pub struct Note {
    pub title: String,
    pub artist: String,
    pub album: Option<String>,
    pub art_url: Option<String>,
}

/// Run the notification actor. `art_dir` is the album-art cache; covers for
/// the popup are written into `out_dir`.
pub async fn run(art_dir: PathBuf, out_dir: PathBuf, mut rx: UnboundedReceiver<Note>) {
    let http = reqwest::Client::builder()
        .timeout(Duration::from_secs(10))
        .build()
        .expect("http client");
    // Notification id to replace, so a new song updates the same popup.
    let mut id: Option<u32> = None;
    // libnotify before 0.7.10 (Ubuntu 22.04, Debian 11) has no --print-id /
    // --replace-id: drop them after the first failure, popups then stack.
    let mut ids = true;
    let mut warned = false;
    // Alternate between two files: the notification server may still be
    // reading the previous cover when the next song starts.
    let mut flip = false;
    let mut next: Option<Note> = None;
    loop {
        let mut note = match next.take() {
            Some(n) => n,
            None => match rx.recv().await {
                Some(n) => n,
                None => break,
            },
        };
        // Skipping quickly through songs: only the newest one matters.
        while let Ok(n) = rx.try_recv() {
            note = n;
        }
        let mut icon = None;
        if let Some(url) = &note.art_url {
            flip = !flip;
            let out = out_dir.join(if flip {
                "notify-cover-a.png"
            } else {
                "notify-cover-b.png"
            });
            icon = cover(&http, &art_dir, url, &out).await;
        }
        // A newer song started while the cover downloaded: show that instead.
        if let Ok(n) = rx.try_recv() {
            next = Some(n);
            continue;
        }
        let mut res = show(&note, icon.as_deref(), id, ids).await;
        if res.is_err() && ids {
            res = show(&note, icon.as_deref(), None, false).await;
            if res.is_ok() {
                tracing::info!("notify-send without --print-id; notifications won't replace each other");
                ids = false;
            }
        }
        match res {
            Ok(new_id) => id = new_id.or(id),
            Err(e) if !warned => {
                warned = true;
                tracing::warn!("notification: {e:#}");
            }
            Err(e) => tracing::debug!("notification: {e:#}"),
        }
    }
}

/// Cover art as a small PNG in `out`, from the art cache (downloading it if
/// needed). None if it can't be had within a few seconds.
async fn cover(http: &reqwest::Client, art_dir: &Path, url: &str, out: &Path) -> Option<PathBuf> {
    let cache = art_dir.join(crate::art::cache_name(url));
    let img = tokio::time::timeout(Duration::from_secs(5), crate::art::load(http, url, cache))
        .await
        .ok()?
        .ok()?;
    let out = out.to_path_buf();
    tokio::task::spawn_blocking(move || {
        let small = img.thumbnail(256, 256);
        let tmp = out.with_extension("png.tmp");
        small.save_with_format(&tmp, image::ImageFormat::Png).ok()?;
        std::fs::rename(&tmp, &out).ok()?;
        Some(out)
    })
    .await
    .ok()
    .flatten()
}

/// Notification servers read the body as simple markup.
fn escape(s: &str) -> String {
    s.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;")
}

fn body(note: &Note) -> String {
    match note.album.as_deref().filter(|a| !a.is_empty()) {
        Some(album) => format!("{}\n{}", escape(&note.artist), escape(album)),
        None => escape(&note.artist),
    }
}

fn args(note: &Note, icon: Option<&Path>, replace: Option<u32>, ids: bool) -> Vec<String> {
    let mut a = vec![
        "--app-name=pova".to_string(),
        "--urgency=low".into(),
        "--expire-time=6000".into(),
        "--hint=string:desktop-entry:pova".into(),
    ];
    if ids {
        a.push("--print-id".into());
        if let Some(id) = replace {
            a.push(format!("--replace-id={id}"));
        }
    }
    if let Some(icon) = icon {
        a.push(format!("--icon={}", icon.display()));
    }
    // `--` so a title starting with "-" isn't read as an option.
    a.push("--".into());
    a.push(note.title.clone());
    a.push(body(note));
    a
}

/// Show `note`; returns the notification id notify-send printed.
async fn show(note: &Note, icon: Option<&Path>, replace: Option<u32>, ids: bool) -> anyhow::Result<Option<u32>> {
    let child = Command::new("notify-send")
        .args(args(note, icon, replace, ids))
        .stdin(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .kill_on_drop(true)
        .output();
    let out = tokio::time::timeout(Duration::from_secs(5), child).await??;
    anyhow::ensure!(out.status.success(), "notify-send failed ({})", out.status);
    Ok(String::from_utf8_lossy(&out.stdout).trim().parse().ok())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn note() -> Note {
        Note {
            title: "-Fine Again".into(),
            artist: "Seether & <Friends>".into(),
            album: Some("Disclaimer".into()),
            art_url: None,
        }
    }

    #[test]
    fn args_escape_markup_and_stop_option_parsing() {
        let a = args(&note(), Some(Path::new("/c/a.png")), Some(7), true);
        assert!(a.contains(&"--replace-id=7".to_string()));
        assert!(a.contains(&"--icon=/c/a.png".to_string()));
        let dash = a.iter().position(|x| x == "--").unwrap();
        assert_eq!(a[dash + 1], "-Fine Again");
        assert_eq!(a[dash + 2], "Seether &amp; &lt;Friends&gt;\nDisclaimer");
        assert_eq!(dash + 3, a.len());
    }

    #[test]
    fn first_notification_has_no_replace_id() {
        let a = args(&Note { album: None, ..note() }, None, None, true);
        assert!(a.contains(&"--print-id".to_string()));
        assert!(
            !a.iter()
                .any(|x| x.starts_with("--replace-id") || x.starts_with("--icon"))
        );
        assert_eq!(a.last().unwrap(), "Seether &amp; &lt;Friends&gt;");
        // Old libnotify: no id options at all.
        let a = args(&note(), None, Some(7), false);
        assert!(!a.iter().any(|x| x.contains("-id")));
    }
}

//! Lyrics from LRCLIB (time-synced when available). The app falls back to
//! YouTube Music's plain lyrics through the ytm actor when LRCLIB has nothing.

use serde::Deserialize;
use tokio::sync::mpsc::{UnboundedReceiver, UnboundedSender};

#[derive(Clone, Debug, PartialEq)]
pub enum Lyrics {
    /// (seconds, line), sorted by time.
    Synced(Vec<(f64, String)>),
    Plain(String),
}

#[derive(Debug)]
pub struct LyricsReq {
    pub video_id: String,
    pub title: String,
    pub artist: String,
    pub album: Option<String>,
    pub duration: Option<f64>,
}

#[derive(Debug)]
pub struct LyricsEvent {
    pub video_id: String,
    pub lyrics: Option<Lyrics>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct LrcLibRecord {
    #[serde(default)]
    duration: Option<f64>,
    #[serde(default)]
    instrumental: bool,
    plain_lyrics: Option<String>,
    synced_lyrics: Option<String>,
}

const API: &str = "https://lrclib.net/api";

pub async fn run(mut reqs: UnboundedReceiver<LyricsReq>, events: UnboundedSender<LyricsEvent>) {
    let http = reqwest::Client::builder()
        .user_agent(concat!("pova/", env!("CARGO_PKG_VERSION"), " (terminal music player)"))
        .timeout(std::time::Duration::from_secs(10))
        .build()
        .expect("http client");
    while let Some(req) = reqs.recv().await {
        let http = http.clone();
        let events = events.clone();
        tokio::spawn(async move {
            let lyrics = match fetch(&http, &req).await {
                Ok(l) => l,
                Err(e) => {
                    tracing::info!("lrclib lookup for {:?} failed: {e:#}", req.title);
                    None
                }
            };
            let _ = events.send(LyricsEvent {
                video_id: req.video_id,
                lyrics,
            });
        });
    }
}

async fn fetch(http: &reqwest::Client, req: &LyricsReq) -> anyhow::Result<Option<Lyrics>> {
    let artist = primary_artist(&req.artist);
    let title = strip_artist_prefix(&clean_title(&req.title), &artist);

    // 1. Exact signature match (needs duration within ±2s).
    if let Some(duration) = req.duration {
        let mut params = vec![
            ("track_name", title.clone()),
            ("artist_name", artist.clone()),
            ("duration", format!("{}", duration.round() as u64)),
        ];
        if let Some(album) = &req.album {
            params.push(("album_name", album.clone()));
        }
        let resp = http.get(format!("{API}/get")).query(&params).send().await?;
        if resp.status().is_success() {
            let rec: LrcLibRecord = resp.json().await?;
            if let Some(l) = to_lyrics(rec) {
                return Ok(Some(l));
            }
        }
    }

    // 2. Fuzzy search; prefer synced results with the closest duration.
    let resp = http
        .get(format!("{API}/search"))
        .query(&[("track_name", title.as_str()), ("artist_name", artist.as_str())])
        .send()
        .await?
        .error_for_status()?;
    let mut recs: Vec<LrcLibRecord> = resp.json().await?;
    let dist = |r: &LrcLibRecord| match (r.duration, req.duration) {
        (Some(a), Some(b)) => (a - b).abs(),
        _ => 30.0,
    };
    recs.retain(|r| dist(r) <= 15.0);
    recs.sort_by(|a, b| {
        let synced = |r: &LrcLibRecord| r.synced_lyrics.as_ref().is_none_or(|s| s.trim().is_empty());
        (synced(a), dist(a))
            .partial_cmp(&(synced(b), dist(b)))
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    Ok(recs.into_iter().find_map(to_lyrics))
}

fn to_lyrics(rec: LrcLibRecord) -> Option<Lyrics> {
    if rec.instrumental {
        return Some(Lyrics::Plain("♪ Instrumental ♪".into()));
    }
    if let Some(synced) = rec.synced_lyrics.as_deref() {
        let lines = parse_lrc(synced);
        if !lines.is_empty() {
            return Some(Lyrics::Synced(lines));
        }
    }
    rec.plain_lyrics.filter(|p| !p.trim().is_empty()).map(Lyrics::Plain)
}

/// Parse LRC text: `[mm:ss.xx]line`, multiple tags per line allowed.
pub fn parse_lrc(text: &str) -> Vec<(f64, String)> {
    let mut out = Vec::new();
    for raw in text.lines() {
        let mut rest = raw.trim();
        let mut times = Vec::new();
        while let Some(stripped) = rest.strip_prefix('[') {
            let Some(end) = stripped.find(']') else { break };
            let tag = &stripped[..end];
            match parse_timestamp(tag) {
                Some(t) => times.push(t),
                None if times.is_empty() => break, // metadata tag like [ar:...]
                None => {}
            }
            rest = &stripped[end + 1..];
        }
        let line = rest.trim().to_string();
        for t in times {
            out.push((t, line.clone()));
        }
    }
    out.sort_by(|a, b| a.0.partial_cmp(&b.0).unwrap_or(std::cmp::Ordering::Equal));
    out
}

fn parse_timestamp(tag: &str) -> Option<f64> {
    let (m, s) = tag.split_once(':')?;
    let m: f64 = m.trim().parse().ok()?;
    let s: f64 = s.trim().replace(':', ".").parse().ok()?;
    Some(m * 60.0 + s)
}

/// Index of the line active at `pos` seconds.
pub fn active_line(lines: &[(f64, String)], pos: f64) -> Option<usize> {
    match lines.partition_point(|(t, _)| *t <= pos) {
        0 => None,
        n => Some(n - 1),
    }
}

/// Strip video-ish suffixes: "Song (Official Video) [HD]" -> "Song".
pub fn clean_title(title: &str) -> String {
    const NOISE: &[&str] = &[
        "official",
        "video",
        "lyric",
        "lyrics",
        "audio",
        "visualizer",
        "visualiser",
        "mv",
        "hd",
        "4k",
        "remaster",
    ];
    let mut out = String::new();
    let mut chars = title.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '(' || c == '[' {
            let close = if c == '(' { ')' } else { ']' };
            let inner: String = chars.by_ref().take_while(|&x| x != close).collect();
            let lower = inner.to_lowercase();
            if !NOISE.iter().any(|n| {
                lower
                    .split_whitespace()
                    .any(|w| w.trim_matches(|c: char| !c.is_alphanumeric()) == *n)
            }) {
                out.push(c);
                out.push_str(&inner);
                out.push(close);
            }
        } else {
            out.push(c);
        }
    }
    out.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// "Artist - Title" style video names: drop the leading artist.
pub fn strip_artist_prefix(title: &str, artist: &str) -> String {
    match title.split_once(" - ") {
        Some((a, t)) if !t.is_empty() && a.trim().eq_ignore_ascii_case(artist.trim()) => t.trim().to_string(),
        _ => title.to_string(),
    }
}

pub fn primary_artist(artists: &str) -> String {
    let a = artists.split([',', '&']).next().unwrap_or(artists);
    let a = a.split(" feat").next().unwrap_or(a);
    let a = a.split(" x ").next().unwrap_or(a);
    a.trim_end_matches(" - Topic").trim().to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lrc_parsing() {
        let l = parse_lrc("[ar:Someone]\n[00:12.50]Hello\n[00:05.00][01:00.00]Chorus\n[00:20.00]\n");
        assert_eq!(
            l,
            vec![
                (5.0, "Chorus".into()),
                (12.5, "Hello".into()),
                (20.0, String::new()),
                (60.0, "Chorus".into()),
            ]
        );
        assert_eq!(active_line(&l, 0.0), None);
        assert_eq!(active_line(&l, 12.5), Some(1));
        assert_eq!(active_line(&l, 30.0), Some(2));
        assert_eq!(active_line(&l, 999.0), Some(3));
    }

    #[test]
    fn title_cleanup() {
        assert_eq!(clean_title("Get Lucky (Official Video) [HD]"), "Get Lucky");
        assert_eq!(clean_title("Song (feat. X)"), "Song (feat. X)");
        assert_eq!(
            strip_artist_prefix(&clean_title("Daft Punk - One More Time (Official Audio)"), "Daft Punk"),
            "One More Time"
        );
        assert_eq!(strip_artist_prefix("Song - Remastered", "Someone"), "Song - Remastered");
        assert_eq!(primary_artist("Daft Punk, Pharrell Williams"), "Daft Punk");
        assert_eq!(primary_artist("Rick Astley - Topic"), "Rick Astley");
    }
}

//! Media controls: pova as an MPRIS player on the D-Bus session bus.
//!
//! MPRIS is the freedesktop standard every Linux desktop's media widget
//! speaks (KDE, GNOME, Cinnamon, XFCE, ...), as do media keys, lock screens,
//! KDE Connect, waybar and `playerctl`.
//!
//! This actor never touches mpv. Calls from the bus become the same
//! [`control::Command`]s the `pova next` socket sends, and the app pushes a
//! [`Snapshot`] only when something visible changes (never per frame).
//! Clients interpolate the position themselves; it's read on request from a
//! shared atomic. Turned off, the bus connection is dropped and the task just
//! waits.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use tokio::sync::mpsc::{UnboundedReceiver, UnboundedSender, unbounded_channel};
use tokio::sync::oneshot;
use zbus::object_server::SignalEmitter;
use zbus::{Connection, fdo, interface};
use zbus::{ObjectPath, OwnedVariant, Variant};

use crate::control::{Command, Request};

const PATH: &str = "/org/mpris/MediaPlayer2";
const NO_TRACK: &str = "/org/mpris/MediaPlayer2/TrackList/NoTrack";

/// Shown when media controls are turned on without a desktop session.
pub const NO_SESSION: &str = "Media controls need a desktop session (a D-Bus session bus), and there \
     isn't one here (ssh or a text console?). They'll work when you run pova from your desktop.";

/// Whether there's a session bus to connect to (it's how desktops find
/// media players). Missing over ssh and on a text console.
pub fn session_bus_available() -> bool {
    if std::env::var_os("DBUS_SESSION_BUS_ADDRESS").is_some_and(|a| !a.is_empty()) {
        return true;
    }
    std::env::var_os("XDG_RUNTIME_DIR").is_some_and(|d| PathBuf::from(d).join("bus").exists())
}

/// The playback position in seconds, as `f64` bits, shared with the app.
pub type Position = Arc<AtomicU64>;

pub fn set_position(pos: &Position, secs: f64) {
    pos.store(secs.to_bits(), Ordering::Relaxed);
}

fn position_us(pos: &Position) -> i64 {
    (f64::from_bits(pos.load(Ordering::Relaxed)).max(0.0) * 1e6) as i64
}

#[derive(Debug)]
pub enum Msg {
    /// Media controls turned on or off (the Esc menu, or at startup).
    Enable(bool),
    State(Snapshot),
    /// The user seeked; the new position in seconds.
    Seeked(f64),
}

#[derive(Debug, Clone, PartialEq, Default)]
pub struct Song {
    pub video_id: String,
    pub title: String,
    pub artist: String,
    pub album: Option<String>,
    pub art_url: Option<String>,
    /// Seconds; 0 while unknown.
    pub length: f64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Status {
    Playing,
    Paused,
    #[default]
    Stopped,
}

impl Status {
    fn as_str(self) -> &'static str {
        match self {
            Status::Playing => "Playing",
            Status::Paused => "Paused",
            Status::Stopped => "Stopped",
        }
    }
}

/// What the desktop shows. Sent by the app when it changes.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct Snapshot {
    pub song: Option<Song>,
    pub status: Status,
    /// 0 to 100, 0 while muted.
    pub volume: f64,
    pub shuffle: bool,
    /// MPRIS LoopStatus: "None", "Playlist" or "Track".
    pub repeat: &'static str,
}

pub async fn run(
    art_dir: PathBuf,
    out_dir: PathBuf,
    pos: Position,
    mut rx: UnboundedReceiver<Msg>,
    control: UnboundedSender<Request>,
) {
    let mut conn: Option<Connection> = None;
    let mut warned = false;
    let mut snap = Snapshot::default();
    // Cover art for the current song, as a local file (some desktops won't
    // load https art), filled in after it's fetched.
    let (art_tx, mut art_rx) = unbounded_channel::<(String, String)>();
    let mut art: Option<(String, String)> = None;
    let mut http: Option<reqwest::Client> = None;
    // Covers written so far, newest last; older ones are deleted.
    let mut covers: std::collections::VecDeque<PathBuf> = Default::default();
    // Only the newest song's cover download runs, so a slow one for an
    // earlier song can't overwrite the file the current song points at.
    let mut fetch: Option<tokio::task::JoinHandle<()>> = None;
    loop {
        tokio::select! {
            msg = rx.recv() => {
                let Some(msg) = msg else { break };
                match msg {
                    Msg::Enable(true) if conn.is_none() => {
                        match connect(&pos, &control, &snap, art.as_ref()).await {
                            Ok(c) => {
                                tracing::info!("media controls on (MPRIS)");
                                conn = Some(c);
                            }
                            // No session bus: a TTY, ssh, a bare window manager.
                            Err(e) if !warned => {
                                warned = true;
                                tracing::warn!("media controls unavailable: {e}");
                            }
                            Err(e) => tracing::debug!("media controls unavailable: {e}"),
                        }
                    }
                    Msg::Enable(true) => {}
                    Msg::Enable(false) => {
                        if conn.take().is_some() {
                            tracing::info!("media controls off");
                        }
                    }
                    Msg::State(new) => {
                        let new_song = new.song.as_ref().map(|s| &s.video_id) != snap.song.as_ref().map(|s| &s.video_id);
                        if new_song {
                            art = None;
                            if let Some(f) = fetch.take() {
                                f.abort();
                            }
                            if let Some(url) = new.song.as_ref().and_then(|s| s.art_url.clone()) {
                                let id = new.song.as_ref().map(|s| s.video_id.clone()).unwrap_or_default();
                                let http = http.get_or_insert_with(|| {
                                    reqwest::Client::builder()
                                        .timeout(Duration::from_secs(10))
                                        .build()
                                        .expect("http client")
                                }).clone();
                                // One file per song, so a late write can only ever touch
                                // its own song's cover. Keep the previous one (a desktop
                                // may still be reading it) and delete older ones.
                                let safe: String = id.chars().filter(|c| c.is_ascii_alphanumeric() || *c == '-' || *c == '_').collect();
                                let out = out_dir.join(format!("mpris-cover-{safe}.png"));
                                if !covers.contains(&out) {
                                    covers.push_back(out.clone());
                                }
                                while covers.len() > 2 {
                                    if let Some(old) = covers.pop_front() {
                                        let _ = std::fs::remove_file(old);
                                    }
                                }
                                let (art_dir, tx) = (art_dir.clone(), art_tx.clone());
                                fetch = Some(tokio::spawn(async move {
                                    if let Some(p) = crate::art::cover_png(&http, &art_dir, &url, &out).await {
                                        let _ = tx.send((id, format!("file://{}", p.display())));
                                    }
                                }));
                            }
                        }
                        let old = std::mem::replace(&mut snap, new);
                        if let Some(c) = &conn
                            && let Err(e) = update(c, &old, &snap, art.as_ref()).await
                        {
                            tracing::debug!("mpris update: {e}");
                        }
                    }
                    Msg::Seeked(secs) => {
                        if let Some(c) = &conn
                            && let Ok(iface) = c.object_server().interface::<_, Player>(PATH).await
                        {
                            let _ = Player::seeked(iface.signal_emitter(), (secs.max(0.0) * 1e6) as i64).await;
                        }
                    }
                }
            }
            Some((id, url)) = art_rx.recv() => {
                // Only if that song is still playing.
                if snap.song.as_ref().map(|s| &s.video_id) == Some(&id) {
                    let old_art = art.replace((id, url));
                    if let Some(c) = &conn
                        && old_art != art
                    {
                        let _ = update(c, &snap, &snap, art.as_ref()).await;
                    }
                }
            }
        }
    }
    for c in covers {
        let _ = std::fs::remove_file(c);
    }
}

async fn connect(
    pos: &Position,
    control: &UnboundedSender<Request>,
    snap: &Snapshot,
    art: Option<&(String, String)>,
) -> zbus::Result<Connection> {
    let player = Player {
        snap: snap.clone(),
        art: art_for(snap, art),
        pos: pos.clone(),
        control: control.clone(),
    };
    // One player per pova instance, like browsers and mpv do.
    let name = format!("org.mpris.MediaPlayer2.pova.instance{}", std::process::id());
    let conn = tokio::time::timeout(
        Duration::from_secs(5),
        zbus::connection::Builder::session()
            .serve_at(PATH, Root)
            .serve_at(PATH, player)
            .name(name)
            .build(),
    )
    .await
    .map_err(|_| zbus::Error::Failure("timed out connecting to the session bus".into()))??;
    Ok(conn)
}

/// The cover file for the current song, if it's been fetched.
fn art_for(snap: &Snapshot, art: Option<&(String, String)>) -> Option<String> {
    let id = &snap.song.as_ref()?.video_id;
    art.filter(|(a, _)| a == id).map(|(_, url)| url.clone())
}

/// Copy `new` into the served player and emit PropertiesChanged for what changed.
async fn update(conn: &Connection, old: &Snapshot, new: &Snapshot, art: Option<&(String, String)>) -> zbus::Result<()> {
    let iface = conn.object_server().interface::<_, Player>(PATH).await?;
    let mut p = iface.get_mut().await;
    let art = art_for(new, art);
    let metadata = old.song != new.song || p.art != art;
    p.snap = new.clone();
    p.art = art;
    let e = iface.signal_emitter();
    if metadata {
        p.metadata_changed(e).await?;
    }
    if old.song.is_some() != new.song.is_some() || metadata {
        p.can_go_next_changed(e).await?;
        p.can_go_previous_changed(e).await?;
        p.can_play_changed(e).await?;
        p.can_pause_changed(e).await?;
        p.can_seek_changed(e).await?;
    }
    if old.status != new.status {
        p.playback_status_changed(e).await?;
    }
    if old.volume != new.volume {
        p.volume_changed(e).await?;
    }
    if old.shuffle != new.shuffle {
        p.shuffle_changed(e).await?;
    }
    if old.repeat != new.repeat {
        p.loop_status_changed(e).await?;
    }
    Ok(())
}

/// `org.mpris.MediaPlayer2`: who we are.
struct Root;

#[interface(name = "org.mpris.MediaPlayer2")]
impl Root {
    /// pova lives in a terminal; there's no window to raise.
    fn raise(&self) {}

    /// Not offered (CanQuit is false): quitting belongs to the terminal.
    fn quit(&self) -> fdo::Result<()> {
        Err(fdo::Error::NotSupported("pova quits from its own window".into()))
    }

    #[zbus(property)]
    fn can_quit(&self) -> bool {
        false
    }

    #[zbus(property)]
    fn can_raise(&self) -> bool {
        false
    }

    #[zbus(property)]
    fn has_track_list(&self) -> bool {
        false
    }

    #[zbus(property)]
    fn identity(&self) -> &str {
        "pova"
    }

    /// Lets desktops show pova's name and icon (pova.desktop).
    #[zbus(property)]
    fn desktop_entry(&self) -> &str {
        "pova"
    }

    #[zbus(property)]
    fn supported_uri_schemes(&self) -> Vec<String> {
        Vec::new()
    }

    #[zbus(property)]
    fn supported_mime_types(&self) -> Vec<String> {
        Vec::new()
    }
}

/// `org.mpris.MediaPlayer2.Player`: playback.
struct Player {
    snap: Snapshot,
    art: Option<String>,
    pos: Position,
    control: UnboundedSender<Request>,
}

impl Player {
    /// Hand a command to the app, like `pova next` does. The answer isn't needed.
    fn send(&self, command: Command) {
        let (reply, _) = oneshot::channel();
        let _ = self.control.send(Request { command, reply });
    }

    fn has_song(&self) -> bool {
        self.snap.song.is_some()
    }
}

#[interface(name = "org.mpris.MediaPlayer2.Player")]
impl Player {
    fn next(&self) {
        self.send(Command::Next);
    }

    fn previous(&self) {
        self.send(Command::Prev);
    }

    fn pause(&self) {
        self.send(Command::Pause);
    }

    fn play_pause(&self) {
        self.send(Command::Toggle);
    }

    /// pova has no "stopped" state of its own; stop pauses.
    fn stop(&self) {
        self.send(Command::Pause);
    }

    fn play(&self) {
        self.send(Command::Play);
    }

    /// `offset` in microseconds.
    fn seek(&self, offset: i64) {
        if self.has_song() {
            self.send(Command::Seek(format!("{:+}", offset as f64 / 1e6)));
        }
    }

    fn set_position(&self, track_id: ObjectPath<'_>, position: i64) {
        // Ignored if it's for a song that's no longer playing (the spec says so).
        let Some(song) = &self.snap.song else { return };
        if track_id.as_str() != track_path(&song.video_id) || position < 0 {
            return;
        }
        if song.length > 0.0 && position as f64 / 1e6 > song.length {
            return;
        }
        self.send(Command::Seek(format!("{}", position as f64 / 1e6)));
    }

    fn open_uri(&self, _uri: &str) -> fdo::Result<()> {
        Err(fdo::Error::NotSupported("use `pova add` or the search tab".into()))
    }

    #[zbus(signal)]
    async fn seeked(emitter: &SignalEmitter<'_>, position: i64) -> zbus::Result<()>;

    #[zbus(property)]
    fn playback_status(&self) -> &str {
        self.snap.status.as_str()
    }

    #[zbus(property)]
    fn loop_status(&self) -> &str {
        if self.snap.repeat.is_empty() {
            "None"
        } else {
            self.snap.repeat
        }
    }

    #[zbus(property)]
    fn set_loop_status(&mut self, value: &str) -> fdo::Result<()> {
        let repeat = match value {
            "None" => "off",
            "Playlist" => "all",
            "Track" => "one",
            other => return Err(fdo::Error::InvalidArgs(format!("unknown loop status {other}"))),
        };
        self.send(Command::Repeat(repeat.into()));
        Ok(())
    }

    #[zbus(property)]
    fn rate(&self) -> f64 {
        1.0
    }

    /// Fixed at 1 for now; requests for other rates are ignored.
    #[zbus(property)]
    fn set_rate(&mut self, _value: f64) {}

    #[zbus(property)]
    fn minimum_rate(&self) -> f64 {
        1.0
    }

    #[zbus(property)]
    fn maximum_rate(&self) -> f64 {
        1.0
    }

    #[zbus(property)]
    fn shuffle(&self) -> bool {
        self.snap.shuffle
    }

    #[zbus(property)]
    fn set_shuffle(&mut self, value: bool) {
        self.send(Command::Shuffle(value));
    }

    #[zbus(property)]
    fn metadata(&self) -> HashMap<String, OwnedVariant> {
        metadata(self.snap.song.as_ref(), self.art.as_deref())
    }

    /// 0.0 to 1.0.
    #[zbus(property)]
    fn volume(&self) -> f64 {
        (self.snap.volume / 100.0).clamp(0.0, 1.0)
    }

    #[zbus(property)]
    fn set_volume(&mut self, value: f64) {
        if value.is_finite() {
            self.send(Command::Volume(format!("{}", (value.clamp(0.0, 1.0) * 100.0).round())));
        }
    }

    /// Read on request; clients interpolate between reads (no signal).
    #[zbus(property(emits_changed_signal = "false"))]
    fn position(&self) -> i64 {
        position_us(&self.pos)
    }

    #[zbus(property)]
    fn can_go_next(&self) -> bool {
        self.has_song()
    }

    #[zbus(property)]
    fn can_go_previous(&self) -> bool {
        self.has_song()
    }

    #[zbus(property)]
    fn can_play(&self) -> bool {
        self.has_song()
    }

    #[zbus(property)]
    fn can_pause(&self) -> bool {
        self.has_song()
    }

    #[zbus(property)]
    fn can_seek(&self) -> bool {
        self.snap.song.as_ref().is_some_and(|s| s.length > 0.0)
    }

    #[zbus(property(emits_changed_signal = "const"))]
    fn can_control(&self) -> bool {
        true
    }
}

/// A D-Bus object path for a song. Video ids use `A-Z a-z 0-9 - _`; object
/// paths don't allow `-`, so it's written as `_2d` (`_` itself as `_5f`).
fn track_path(video_id: &str) -> String {
    let mut s = String::from("/org/pova/track/");
    for c in video_id.chars() {
        match c {
            'A'..='Z' | 'a'..='z' | '0'..='9' => s.push(c),
            c => s.push_str(&format!("_{:02x}", c as u32 & 0xff)),
        }
    }
    s
}

fn metadata(song: Option<&Song>, art: Option<&str>) -> HashMap<String, OwnedVariant> {
    let mut m = HashMap::new();
    let mut put = |k: &str, v: Variant<'_>| {
        if let Ok(v) = v.try_to_owned() {
            m.insert(k.to_string(), v);
        }
    };
    let Some(s) = song else {
        if let Ok(p) = ObjectPath::try_from(NO_TRACK) {
            put("mpris:trackid", Variant::from(p));
        }
        return m;
    };
    if let Ok(p) = ObjectPath::try_from(track_path(&s.video_id)) {
        put("mpris:trackid", Variant::from(p));
    }
    if s.length > 0.0 {
        put("mpris:length", Variant::from((s.length * 1e6) as i64));
    }
    put("xesam:title", Variant::from(s.title.as_str()));
    put("xesam:artist", Variant::from(vec![s.artist.as_str()]));
    if let Some(album) = s.album.as_deref().filter(|a| !a.is_empty()) {
        put("xesam:album", Variant::from(album));
    }
    put(
        "xesam:url",
        Variant::from(format!("https://music.youtube.com/watch?v={}", s.video_id)),
    );
    // The local cover once fetched, the web one until then.
    if let Some(url) = art.or(s.art_url.as_deref()) {
        put("mpris:artUrl", Variant::from(url));
    }
    m
}

#[cfg(test)]
mod tests {
    use super::*;

    fn song() -> Song {
        Song {
            video_id: "a-B_9".into(),
            title: "Fine Again".into(),
            artist: "Seether".into(),
            album: Some("Disclaimer".into()),
            art_url: Some("https://lh3.example/x".into()),
            length: 244.5,
        }
    }

    #[test]
    fn track_ids_are_valid_object_paths() {
        let p = track_path("a-B_9");
        assert_eq!(p, "/org/pova/track/a_2dB_5f9");
        assert!(ObjectPath::try_from(p.as_str()).is_ok());
        assert_ne!(track_path("a-b"), track_path("a_b"));
    }

    #[test]
    fn metadata_has_what_widgets_need() {
        let m = metadata(Some(&song()), None);
        let s = |k: &str| String::try_from(m[k].clone()).unwrap();
        assert_eq!(s("xesam:title"), "Fine Again");
        assert_eq!(s("xesam:album"), "Disclaimer");
        assert_eq!(s("mpris:artUrl"), "https://lh3.example/x");
        assert_eq!(i64::try_from(m["mpris:length"].clone()).unwrap(), 244_500_000);
        let artists = <Vec<String>>::try_from(m["xesam:artist"].clone()).unwrap();
        assert_eq!(artists, vec!["Seether".to_string()]);
        // The local cover wins once it's there.
        let m = metadata(Some(&song()), Some("file:///c/mpris-cover-a.png"));
        assert_eq!(
            String::try_from(m["mpris:artUrl"].clone()).unwrap(),
            "file:///c/mpris-cover-a.png"
        );
        // Nothing playing: only the NoTrack id.
        let m = metadata(None, None);
        assert_eq!(m.len(), 1);
    }

    #[test]
    fn position_round_trips_through_the_atomic() {
        let pos: Position = Arc::default();
        set_position(&pos, 12.25);
        assert_eq!(position_us(&pos), 12_250_000);
        set_position(&pos, -1.0);
        assert_eq!(position_us(&pos), 0);
    }
}

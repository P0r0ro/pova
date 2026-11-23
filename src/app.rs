//! Application state and the reducer that reacts to input and actor events.

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::time::{Duration, Instant, SystemTime};

use crossterm::event::{Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers, MouseButton, MouseEvent, MouseEventKind};
use ratatui::layout::{Position, Rect};
use ratatui::widgets::TableState;
use ratatui_image::picker::Picker;
use ratatui_image::thread::ThreadProtocol;
use tokio::sync::mpsc::UnboundedSender;

use crate::art::{ArtEvent, ArtReq};
use crate::config::{Config, Paths, State, VizMode};
use crate::keymap::{Action, Keymap};
use crate::lyrics::{Lyrics, LyricsEvent, LyricsReq};
use crate::model::{Item, Track, video_id_from_url};
use crate::player::mpv::{PlayerCmd, PlayerEvent};
use crate::player::queue::{Queue, Repeat};
use crate::resolve::{ResolveReq, Resolved};
use crate::theme::{self, Theme};
use crate::viz::Frame;
use crate::ytm::{SearchFilter, Suggestion, View, YtmEvent, YtmReq};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Tab {
    NowPlaying,
    Queue,
    Lyrics,
    Search,
    Library,
}

impl Tab {
    pub const ALL: [Tab; 5] = [Tab::NowPlaying, Tab::Queue, Tab::Lyrics, Tab::Search, Tab::Library];
    /// (label, index of the hotkey letter within the label)
    pub fn label(self) -> (&'static str, usize) {
        match self {
            Tab::NowPlaying => ("now playing", 0),
            Tab::Queue => ("queue", 0),
            Tab::Lyrics => ("lyrics", 0),
            Tab::Search => ("search", 0),
            Tab::Library => ("library", 2),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LibSection {
    Playlists,
    Liked,
    History,
}

impl LibSection {
    pub const ALL: [LibSection; 3] = [LibSection::Playlists, LibSection::Liked, LibSection::History];
    pub fn label(self) -> &'static str {
        match self {
            LibSection::Playlists => "Playlists",
            LibSection::Liked => "Liked songs",
            LibSection::History => "History",
        }
    }
    fn view(self) -> View {
        match self {
            LibSection::Playlists => View::LibraryPlaylists,
            LibSection::Liked => View::LikedSongs,
            LibSection::History => View::History,
        }
    }
}

/// A loaded (or loading) list of items.
pub struct ListView {
    pub view: View,
    pub title: String,
    pub items: Vec<Item>,
    pub state: TableState,
    pub token: u64,
    pub loading: bool,
    pub error: Option<String>,
}

impl ListView {
    fn new(view: View, token: u64) -> ListView {
        ListView {
            view,
            title: String::new(),
            items: Vec::new(),
            state: TableState::default(),
            token,
            loading: true,
            error: None,
        }
    }
    pub fn selected(&self) -> Option<&Item> {
        self.state.selected().and_then(|i| self.items.get(i))
    }
    fn tracks(&self) -> Vec<Track> {
        self.items.iter().filter_map(Item::as_track).cloned().collect()
    }
}

#[derive(Default)]
pub struct SearchState {
    pub input: String,
    /// Cursor position in chars.
    pub cursor: usize,
    pub editing: bool,
    pub filter: SearchFilter,
    pub stack: Vec<ListView>,
    /// Autocomplete entries for `suggest_for`.
    pub suggestions: Vec<Suggestion>,
    pub suggest_for: String,
    /// Highlighted suggestion (↑/↓); None = the typed text.
    pub suggest_sel: Option<usize>,
    /// When to ask YTM for suggestions (debounced while typing).
    pub suggest_due: Option<Instant>,
}

impl SearchState {
    /// Suggestions are shown only while typing and only for the current text.
    pub fn visible_suggestions(&self) -> &[Suggestion] {
        if self.editing && !self.input.trim().is_empty() && self.suggest_for == self.input.trim() {
            &self.suggestions
        } else {
            &[]
        }
    }

    /// Inline completion: the rest of the top (or selected) suggestion when it
    /// extends what was typed, case-insensitively.
    pub fn ghost(&self) -> Option<&str> {
        let list = self.visible_suggestions();
        let top = list.get(self.suggest_sel.unwrap_or(0))?;
        if self.cursor != self.input.chars().count() {
            return None;
        }
        let typed = self.input.to_lowercase();
        let cand = top.text.to_lowercase();
        if cand.len() > typed.len() && cand.starts_with(&typed) && top.text.is_char_boundary(typed.len()) {
            Some(&top.text[typed.len()..])
        } else {
            None
        }
    }
}

pub struct LibraryState {
    pub section: LibSection,
    pub stack: Vec<ListView>,
}

pub enum LyricsState {
    None,
    Loading { video_id: String },
    Found { lyrics: Lyrics, source: &'static str },
    Missing,
}

#[derive(Default, Clone)]
pub struct PlayerState {
    pub time: f64,
    pub duration: f64,
    pub paused: bool,
    pub volume: f64,
    pub muted: bool,
    pub idle: bool,
    pub buffering: bool,
    pub ready: bool,
}

pub enum Overlay {
    Help {
        scroll: u16,
    },
    Themes {
        names: Vec<(String, Option<PathBuf>)>,
        state: TableState,
        original: Theme,
    },
    Menu {
        state: TableState,
    },
}

/// Entries of the Esc menu (btop-style options).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MenuItem {
    Themes,
    VizMode,
    Autoplay,
    AlbumArt,
    Help,
    Quit,
}

impl MenuItem {
    pub const ALL: [MenuItem; 6] = [
        MenuItem::Themes,
        MenuItem::VizMode,
        MenuItem::Autoplay,
        MenuItem::AlbumArt,
        MenuItem::Help,
        MenuItem::Quit,
    ];
}

#[derive(Clone, Copy, Debug)]
pub enum Hit {
    Tab(Tab),
    Action(Action),
    Progress,
    List,
    SearchFilter(SearchFilter),
    LibSection(LibSection),
    Suggestion(usize),
    CloseError,
}

/// Clickable regions, rebuilt every frame.
#[derive(Default)]
pub struct HitMap {
    pub regions: Vec<(Rect, Hit)>,
    /// Inner area of the list currently on screen and its first visible row.
    pub list: Option<(Rect, usize)>,
}

impl HitMap {
    pub fn add(&mut self, rect: Rect, hit: Hit) {
        self.regions.push((rect, hit));
    }
    fn at(&self, x: u16, y: u16) -> Option<(Rect, Hit)> {
        self.regions
            .iter()
            .rev()
            .find(|(r, _)| r.contains(Position { x, y }))
            .copied()
    }
}

pub struct Flash {
    pub text: String,
    pub error: bool,
    pub until: Instant,
}

#[derive(Clone, Copy, Debug)]
enum Pending {
    /// Fill the list view carrying this token.
    View,
    /// Enqueue the loaded tracks.
    Enqueue(EnqueueMode),
    /// Play the first track of the result (`--play`).
    PlayFirst,
    /// The signed-in user's liked songs, for like status everywhere.
    LikedSet,
}

/// Is `track` liked? Known statuses (`likes`) win over what the list said.
pub fn is_liked(likes: &HashMap<String, bool>, track: &Track) -> bool {
    likes.get(&track.video_id).copied().or(track.liked).unwrap_or(false)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EnqueueMode {
    Append,
    Next,
    PlayNow,
}

enum RadioMode {
    /// Automatic continuation when the queue runs out.
    Autoplay,
    /// User pressed `r`: replace the queue with seed + radio.
    Start,
}

pub struct Channels {
    pub player: UnboundedSender<PlayerCmd>,
    pub ytm: UnboundedSender<YtmReq>,
    pub lyrics: UnboundedSender<LyricsReq>,
    pub art: UnboundedSender<ArtReq>,
    pub resolve: UnboundedSender<ResolveReq>,
}

pub struct App {
    pub cfg: Config,
    pub paths: Paths,
    pub theme: Theme,
    pub keymap: Keymap,
    pub tab: Tab,
    pub show_viz: bool,
    pub show_player: bool,
    pub show_controls: bool,
    pub viz_mode: VizMode,
    pub viz_frame: Frame,
    pub queue: Queue,
    pub queue_state: TableState,
    pub player: PlayerState,
    pub search: SearchState,
    pub library: LibraryState,
    pub lyrics: LyricsState,
    pub lyrics_scroll: Option<u16>,
    /// Scroll position the synced view chose last frame.
    pub lyrics_auto: u16,
    pub art: Option<ThreadProtocol>,
    /// Sad-cat mascot for tracks without lyrics.
    pub cat_art: Option<ThreadProtocol>,
    pub art_key: Option<String>,
    pub art_loading: bool,
    pub picker: Option<Picker>,
    pub flash: Option<Flash>,
    pub logged_in: bool,
    /// The first sign-in report arrived (later ones are re-logins).
    auth_seen: bool,
    /// Like status per video id, from Liked songs + lists + likes made here.
    pub likes: HashMap<String, bool>,
    /// Liked songs finished loading (unknown status then means "not liked").
    pub likes_loaded: bool,
    pub overlay: Option<Overlay>,
    pub hits: HitMap,
    pub quit: bool,
    pub dirty: bool,
    tx: Channels,
    next_sent: Option<String>,
    next_token: u64,
    pending: HashMap<u64, Pending>,
    radio: HashMap<String, RadioMode>,
    autoplay_requested: Option<String>,
    last_click: Option<(Instant, u16, u16)>,
    load_started: Option<Instant>,
    lyrics_requested: Option<String>,
    /// Pre-resolved direct audio URLs: video id -> (url, use-before).
    streams: HashMap<String, (String, SystemTime)>,
    /// Video ids sent to the resolver and not answered yet.
    resolving: HashSet<String>,
    /// Direct stream URL -> video id (mpv reports the URL as `path`).
    url_vid: HashMap<String, String>,
    /// Selected list track and since when (prefetch after a short dwell).
    dwell: Option<(String, Instant)>,
    dwell_fired: Option<String>,
    /// Current track was started from a pre-resolved URL.
    direct_loaded: Option<String>,
    /// Tracks whose direct URL failed once (fall back to mpv's yt-dlp hook).
    direct_failed: HashSet<String>,
    /// For the "track started in N ms" log line.
    start_timer: Option<(Instant, bool)>,
}

impl App {
    pub fn new(
        cfg: Config,
        paths: Paths,
        state: State,
        keymap: Keymap,
        tx: Channels,
        picker: Option<Picker>,
        art: Option<ThreadProtocol>,
    ) -> App {
        let mut cfg = cfg;
        cfg.autoplay = state.autoplay.unwrap_or(cfg.autoplay);
        cfg.album_art = state.album_art.unwrap_or(cfg.album_art);
        let theme_name = state.theme.clone().unwrap_or_else(|| cfg.theme.clone());
        let theme = theme::load_named(&paths.config_dir, &theme_name, cfg.theme_background);
        let volume = state.volume.unwrap_or(cfg.volume).min(100) as f64;
        App {
            theme,
            keymap,
            tab: Tab::NowPlaying,
            show_viz: state.show_viz.unwrap_or(true),
            show_player: state.show_player.unwrap_or(true),
            show_controls: state.show_controls.unwrap_or(true),
            viz_mode: state.viz_mode.unwrap_or(cfg.viz.mode),
            viz_frame: Frame::default(),
            queue: Queue::default(),
            queue_state: TableState::default(),
            player: PlayerState {
                volume,
                idle: true,
                ..Default::default()
            },
            search: SearchState::default(),
            library: LibraryState {
                section: LibSection::Playlists,
                stack: Vec::new(),
            },
            lyrics: LyricsState::None,
            lyrics_scroll: None,
            lyrics_auto: 0,
            art,
            cat_art: None,
            art_key: None,
            art_loading: false,
            picker,
            flash: None,
            auth_seen: false,
            logged_in: false,
            likes: HashMap::new(),
            likes_loaded: false,
            overlay: None,
            hits: HitMap::default(),
            quit: false,
            dirty: true,
            tx,
            next_sent: None,
            next_token: 0,
            pending: HashMap::new(),
            radio: HashMap::new(),
            autoplay_requested: None,
            last_click: None,
            load_started: None,
            lyrics_requested: None,
            streams: HashMap::new(),
            resolving: HashSet::new(),
            url_vid: HashMap::new(),
            dwell: None,
            dwell_fired: None,
            direct_loaded: None,
            direct_failed: HashSet::new(),
            start_timer: None,
            cfg,
            paths,
        }
    }

    // ───────────────────────────── helpers ─────────────────────────────

    pub fn flash(&mut self, text: impl Into<String>, error: bool) {
        let text = text.into();
        if error {
            tracing::warn!("{text}");
        }
        // Errors stay long enough to read: 8 s plus a second per ~15 characters.
        let secs = if error {
            (8 + text.chars().count() as u64 / 15).min(30)
        } else {
            3
        };
        self.flash = Some(Flash {
            text,
            error,
            until: Instant::now() + Duration::from_secs(secs),
        });
        self.dirty = true;
    }

    fn token(&mut self) -> u64 {
        self.next_token += 1;
        self.next_token
    }

    fn player(&self, cmd: PlayerCmd) {
        let _ = self.tx.player.send(cmd);
    }

    pub fn current(&self) -> Option<&Track> {
        self.queue.current_track()
    }

    /// Label of a menu entry, including the current value of settings.
    pub fn menu_label(&self, item: MenuItem) -> String {
        let on_off = |b: bool| if b { "on" } else { "off" };
        match item {
            MenuItem::Themes => format!("Theme        ‹ {} ›", self.theme.name),
            MenuItem::VizMode => format!("Visualizer   ‹ {} ›", self.viz_mode.label()),
            MenuItem::Autoplay => format!("Autoplay     ‹ {} ›", on_off(self.cfg.autoplay)),
            MenuItem::AlbumArt => format!("Album art    ‹ {} ›", on_off(self.cfg.album_art)),
            MenuItem::Help => "Help".into(),
            MenuItem::Quit => "Quit".into(),
        }
    }

    fn activate_menu(&mut self, item: MenuItem) {
        match item {
            MenuItem::Themes => self.open_themes(),
            MenuItem::Help => self.overlay = Some(Overlay::Help { scroll: 0 }),
            MenuItem::Quit => self.quit = true,
            MenuItem::VizMode => {
                self.viz_mode = self.viz_mode.next();
                self.save_state();
            }
            MenuItem::Autoplay => {
                self.cfg.autoplay = !self.cfg.autoplay;
                if self.cfg.autoplay {
                    self.autoplay_requested = None;
                    self.maybe_autoplay();
                }
                self.save_state();
            }
            MenuItem::AlbumArt => {
                self.cfg.album_art = !self.cfg.album_art;
                if self.cfg.album_art {
                    self.art_key = None;
                    self.request_art();
                }
                self.save_state();
            }
        }
    }

    pub fn save_state(&self) {
        State {
            theme: Some(self.theme.name.clone()),
            volume: Some(self.player.volume.round() as u8),
            viz_mode: Some(self.viz_mode),
            show_viz: Some(self.show_viz),
            show_player: Some(self.show_player),
            show_controls: Some(self.show_controls),
            autoplay: Some(self.cfg.autoplay),
            album_art: Some(self.cfg.album_art),
        }
        .save(&self.paths.state_file());
    }

    /// Something on screen is animating (spinners) and needs periodic redraws.
    pub fn animating(&self) -> bool {
        self.art_loading
            || self.player.buffering
            || self.load_started.is_some()
            || matches!(self.lyrics, LyricsState::Loading { .. })
            || self.search.stack.iter().chain(&self.library.stack).any(|l| l.loading)
    }

    /// Per-frame housekeeping. Returns true if a redraw is needed.
    pub fn tick(&mut self) -> bool {
        self.update_dwell();
        if let Some(due) = self.search.suggest_due
            && Instant::now() >= due
        {
            self.search.suggest_due = None;
            let query = self.search.input.trim().to_string();
            if self.search.editing && !query.is_empty() && query != self.search.suggest_for {
                let _ = self.tx.ytm.send(YtmReq::Suggest { query });
            }
        }
        if let Some(f) = &self.flash
            && Instant::now() > f.until
        {
            self.flash = None;
            self.dirty = true;
        }
        std::mem::take(&mut self.dirty)
    }

    // ───────────────────────────── playback ─────────────────────────────

    fn play_index(&mut self, i: usize) {
        if i >= self.queue.len() {
            return;
        }
        self.queue.current = Some(i);
        let Some(track) = self.queue.get(i).cloned() else {
            return;
        };
        let (url, direct) = self.play_url(&track);
        self.direct_loaded = direct.then(|| track.video_id.clone());
        self.start_timer = Some((Instant::now(), direct));
        let next = self.desired_next();
        self.next_sent = next.clone();
        self.player(PlayerCmd::Load { url, next });
        self.player.time = 0.0;
        self.player.duration = self.queue.get(i).and_then(|t| t.duration).unwrap_or(0.0);
        self.load_started = Some(Instant::now());
        self.on_track_changed();
    }

    /// URL mpv should preload after the current track.
    fn desired_next(&mut self) -> Option<String> {
        if self.queue.repeat == Repeat::One {
            return None; // mpv loops the file itself (loop-file)
        }
        let n = self.queue.next_index()?;
        if Some(n) == self.queue.current {
            return None;
        }
        let track = self.queue.get(n)?.clone();
        Some(self.play_url(&track).0)
    }

    /// Pre-resolved direct URL if fresh, else the YouTube Music link (mpv's
    /// yt-dlp hook resolves it, ~3 s). Returns (url, is_direct).
    fn play_url(&mut self, track: &Track) -> (String, bool) {
        let fresh = self
            .streams
            .get(&track.video_id)
            .filter(|(_, until)| *until > SystemTime::now() && !self.direct_failed.contains(&track.video_id))
            .map(|(u, _)| u.clone());
        match fresh {
            Some(url) => {
                self.url_vid.insert(url.clone(), track.video_id.clone());
                (url, true)
            }
            None => (track.url(), false),
        }
    }

    /// Ask the resolver for a direct URL ahead of time (no-op if cached/pending).
    pub fn prefetch(&mut self, video_id: &str) {
        let fresh = self
            .streams
            .get(video_id)
            .is_some_and(|(_, until)| *until > SystemTime::now());
        // Keep the resolver queue short: it's a courtesy, not a crawler.
        if fresh
            || self.direct_failed.contains(video_id)
            || self.resolving.contains(video_id)
            || self.resolving.len() >= 4
        {
            return;
        }
        self.resolving.insert(video_id.to_string());
        let _ = self.tx.resolve.send(ResolveReq {
            video_id: video_id.to_string(),
        });
    }

    /// Prefetch the next two tracks of the queue.
    fn prefetch_upcoming(&mut self) {
        let Some(n) = self.queue.next_index() else { return };
        let ids: Vec<String> = [Some(n), (n + 1 < self.queue.len()).then_some(n + 1)]
            .into_iter()
            .flatten()
            .filter(|&i| Some(i) != self.queue.current)
            .filter_map(|i| self.queue.get(i).map(|t| t.video_id.clone()))
            .collect();
        for id in ids {
            self.prefetch(&id);
        }
    }

    pub fn on_resolved(&mut self, ev: Resolved) {
        self.resolving.remove(&ev.video_id);
        if let Some(stream) = ev.stream {
            self.streams.insert(ev.video_id, stream);
            // Upgrade mpv's preloaded next entry to the direct URL.
            self.sync_next();
        }
    }

    /// Prefetch the track the user has been resting on in a list.
    fn update_dwell(&mut self) {
        let selected = match self.tab {
            Tab::Search => self
                .search
                .stack
                .last()
                .and_then(|l| l.selected())
                .and_then(Item::as_track),
            Tab::Library => self
                .library
                .stack
                .last()
                .and_then(|l| l.selected())
                .and_then(Item::as_track),
            Tab::Queue => self.queue_state.selected().and_then(|i| self.queue.get(i)),
            _ => None,
        }
        .map(|t| t.video_id.clone());
        match (&self.dwell, selected) {
            (Some((id, since)), Some(sel)) if *id == sel => {
                if since.elapsed() >= Duration::from_millis(500) && self.dwell_fired.as_deref() != Some(&sel) {
                    self.dwell_fired = Some(sel.clone());
                    self.prefetch(&sel);
                }
            }
            (_, Some(sel)) => self.dwell = Some((sel, Instant::now())),
            (_, None) => self.dwell = None,
        }
    }

    /// Make mpv's "next" entry match the queue.
    fn sync_next(&mut self) {
        if self.queue.current.is_none() || self.player.idle && self.load_started.is_none() {
            return;
        }
        self.prefetch_upcoming();
        let want = self.desired_next();
        if want != self.next_sent {
            self.next_sent = want.clone();
            self.player(PlayerCmd::SetNext(want));
        }
    }

    fn on_track_changed(&mut self) {
        self.dirty = true;
        self.prefetch_upcoming();
        self.lyrics_scroll = None;
        let Some(track) = self.current().cloned() else { return };
        // Lyrics
        if self.lyrics_requested.as_deref() != Some(&track.video_id) {
            self.lyrics_requested = Some(track.video_id.clone());
            self.lyrics = LyricsState::Loading {
                video_id: track.video_id.clone(),
            };
            let _ = self.tx.lyrics.send(LyricsReq {
                video_id: track.video_id.clone(),
                title: track.title.clone(),
                artist: track.artists.clone(),
                album: track.album.clone(),
                duration: track.duration,
            });
        }
        self.request_art();
        self.maybe_autoplay();
    }

    /// Fetch album art for the current track (if enabled and not shown yet).
    fn request_art(&mut self) {
        let Some(track) = self.current() else { return };
        if !self.cfg.album_art || self.picker.is_none() {
            return;
        }
        let url = track.art_url();
        if self.art_key.as_deref() != Some(&url) {
            self.art_key = Some(url.clone());
            self.art_loading = true;
            if let Some(a) = self.art.as_mut() {
                a.empty_protocol();
            }
            let _ = self.tx.art.send(ArtReq { key: url.clone(), url });
        }
    }

    /// When the last track starts, fetch a radio to keep the music going.
    fn maybe_autoplay(&mut self) {
        if !self.cfg.autoplay || self.queue.repeat != Repeat::Off {
            return;
        }
        let (Some(cur), Some(track)) = (self.queue.current, self.current()) else {
            return;
        };
        if cur + 1 < self.queue.len() {
            return;
        }
        let seed = track.video_id.clone();
        if self.autoplay_requested.as_deref() == Some(&seed) {
            return;
        }
        self.autoplay_requested = Some(seed.clone());
        self.radio.insert(seed.clone(), RadioMode::Autoplay);
        let _ = self.tx.ytm.send(YtmReq::Radio { seed });
    }

    fn next_track(&mut self) {
        match self.queue.skip_index() {
            Some(i) => self.play_index(i),
            None => {
                if self.cfg.autoplay && self.current().is_some() {
                    self.flash("Loading more songs…", false);
                    self.maybe_autoplay();
                } else {
                    self.flash("End of queue", false);
                }
            }
        }
    }

    fn prev_track(&mut self) {
        if self.player.time > 3.0 || self.queue.prev_index().is_none() {
            self.player(PlayerCmd::SeekAbsolute(0.0));
        } else if let Some(i) = self.queue.prev_index() {
            self.play_index(i);
        }
    }

    fn enqueue(&mut self, tracks: Vec<Track>, mode: EnqueueMode) {
        if tracks.is_empty() {
            return;
        }
        let n = tracks.len();
        let was_empty = self.queue.current.is_none();
        match mode {
            EnqueueMode::Append => {
                self.queue.append(tracks);
                self.flash(
                    format!("Added {n} track{} to queue", if n == 1 { "" } else { "s" }),
                    false,
                );
            }
            EnqueueMode::Next => {
                self.queue.insert_next(tracks);
                self.flash(
                    format!("Playing next: {n} track{}", if n == 1 { "" } else { "s" }),
                    false,
                );
            }
            EnqueueMode::PlayNow => {
                self.queue.insert_next(tracks);
                let i = self.queue.current.map_or(0, |c| c + 1);
                self.play_index(i);
                return;
            }
        }
        if was_empty {
            self.play_index(0);
        } else {
            self.sync_next();
        }
    }

    fn set_repeat(&mut self, r: Repeat) {
        self.queue.repeat = r;
        self.player(PlayerCmd::SetLoopFile(r == Repeat::One));
        self.sync_next();
        self.flash(format!("Repeat: {}", r.label()), false);
    }

    // ───────────────────────────── events ─────────────────────────────

    pub fn on_player(&mut self, ev: PlayerEvent) {
        match ev {
            PlayerEvent::Ready => {
                self.player.ready = true;
                self.player(PlayerCmd::SetVolume(self.player.volume));
            }
            PlayerEvent::TimePos(t) => {
                self.player.time = t;
                if t > 0.0
                    && let Some((t0, direct)) = self.start_timer.take()
                {
                    let how = if direct { "pre-resolved URL" } else { "yt-dlp hook" };
                    tracing::info!("track started in {} ms ({how})", t0.elapsed().as_millis());
                }
                if matches!(
                    self.lyrics,
                    LyricsState::Found {
                        lyrics: Lyrics::Synced(_),
                        ..
                    }
                ) || self.tab == Tab::NowPlaying
                    || self.show_controls
                {
                    self.dirty = true;
                }
                return;
            }
            PlayerEvent::Duration(d) => self.player.duration = d,
            PlayerEvent::Paused(p) => self.player.paused = p,
            PlayerEvent::Volume(v) => self.player.volume = v,
            PlayerEvent::Muted(m) => self.player.muted = m,
            PlayerEvent::Buffering(b) => self.player.buffering = b,
            PlayerEvent::Path(Some(path)) => {
                self.load_started = None;
                let Some(vid) = video_id_from_url(&path).or_else(|| self.url_vid.get(&path).cloned()) else {
                    return;
                };
                if self.current().map(|t| t.video_id.as_str()) != Some(vid.as_str()) {
                    // mpv advanced to the preloaded next track (gapless).
                    let idx = self
                        .queue
                        .next_index()
                        .filter(|&n| self.queue.get(n).is_some_and(|t| t.video_id == vid))
                        .or_else(|| self.queue.position_of(&vid));
                    if let Some(i) = idx {
                        self.queue.current = Some(i);
                        self.player.time = 0.0;
                        self.on_track_changed();
                    }
                }
                // Whatever was preloaded is now playing (or stale).
                self.next_sent = None;
                self.sync_next();
            }
            PlayerEvent::Path(None) => {}
            PlayerEvent::Idle(idle) => {
                let was_playing = !self.player.idle;
                self.player.idle = idle;
                if idle && was_playing && self.load_started.is_none() {
                    // Fell off the end of mpv's playlist (end of queue, or the
                    // preloaded track failed). Continue from the queue.
                    self.next_sent = None;
                    match self.queue.next_index() {
                        Some(n) if Some(n) != self.queue.current || self.queue.repeat != Repeat::Off => {
                            self.play_index(n)
                        }
                        _ => self.player.time = 0.0,
                    }
                }
            }
            PlayerEvent::EndFile { reason, error } => {
                if reason == "error" {
                    // A pre-resolved URL can go stale; retry once via yt-dlp.
                    if let (Some(direct), Some(cur)) = (self.direct_loaded.clone(), self.queue.current)
                        && self.current().is_some_and(|t| t.video_id == direct)
                        && self.direct_failed.insert(direct.clone())
                    {
                        tracing::info!("direct stream for {direct} failed; retrying via yt-dlp hook");
                        self.play_index(cur);
                        return;
                    }
                    self.load_started = None;
                    let what = self.current().map(|t| t.title.clone()).unwrap_or_default();
                    self.flash(
                        format!("Can't play {what}: {}", error.unwrap_or_else(|| "unknown error".into())),
                        true,
                    );
                    // mpv goes idle next; make sure we skip instead of retrying.
                    if self.player.idle
                        && let Some(n) = self.queue.skip_index()
                    {
                        self.play_index(n);
                    }
                }
            }
            PlayerEvent::Error(e) => self.flash(format!("mpv: {e}"), true),
            PlayerEvent::Exited => {
                self.flash("mpv exited — playback unavailable", true);
                self.player.ready = false;
            }
        }
        self.dirty = true;
    }

    pub fn on_ytm(&mut self, ev: YtmEvent) {
        self.dirty = true;
        match ev {
            YtmEvent::Suggestions { query, items } => {
                // Ignore stale answers for text that has since changed.
                if query == self.search.input.trim() {
                    self.search.suggest_for = query;
                    self.search.suggestions = items;
                    self.search.suggest_sel = None;
                }
            }
            YtmEvent::Auth { logged_in, message } => {
                let was = self.logged_in;
                self.logged_in = logged_in;
                if !logged_in {
                    self.likes_loaded = false;
                }
                if logged_in && !was && self.auth_seen {
                    self.flash("Signed in to YouTube Music", false);
                }
                self.auth_seen = true;
                if logged_in {
                    // Learn what's already liked so ♥ shows and `f` toggles correctly.
                    let token = self.token();
                    self.pending.insert(token, Pending::LikedSet);
                    let _ = self.tx.ytm.send(YtmReq::Load {
                        token,
                        view: View::LikedSongs,
                    });
                }
                if let Some(m) = message {
                    self.flash(m, true);
                }
            }
            YtmEvent::Loaded { token, title, items } => {
                if self.logged_in {
                    // Signed-in lists carry the user's like status; remember it.
                    for t in items.iter().filter_map(Item::as_track) {
                        if let Some(l) = t.liked {
                            self.likes.insert(t.video_id.clone(), l);
                        }
                    }
                }
                self.on_loaded(token, title, items);
            }
            YtmEvent::Failed { token, message } => {
                if let Some(token) = token {
                    if let Some(Pending::LikedSet) = self.pending.remove(&token) {
                        // Don't block `f` forever; fall back to known statuses.
                        tracing::warn!("loading liked songs failed: {message}");
                        self.likes_loaded = true;
                        return;
                    }
                    if let Some(lv) = self.find_view(token) {
                        lv.loading = false;
                        if lv.title.is_empty() {
                            lv.title = "Couldn't load".into();
                        }
                        lv.error = Some(message);
                        return;
                    }
                }
                self.flash(message, true);
            }
            YtmEvent::Radio { seed, tracks } => {
                let mode = self.radio.remove(&seed);
                match mode {
                    Some(RadioMode::Autoplay) => {
                        let fresh: Vec<Track> = tracks
                            .into_iter()
                            .filter(|t| !self.queue.contains(&t.video_id))
                            .collect();
                        if fresh.is_empty() {
                            return;
                        }
                        let at_end = self.player.idle && self.load_started.is_none();
                        let resume = self.queue.current.map(|c| c + 1);
                        self.queue.append(fresh);
                        if at_end {
                            if let Some(i) = resume {
                                self.play_index(i);
                            }
                        } else {
                            self.sync_next();
                        }
                    }
                    Some(RadioMode::Start) => {
                        let seed_track = self
                            .queue
                            .entries
                            .iter()
                            .map(|e| &e.track)
                            .chain(self.all_list_tracks())
                            .find(|t| t.video_id == seed)
                            .cloned();
                        let mut list: Vec<Track> = seed_track.into_iter().collect();
                        list.extend(tracks);
                        if !list.is_empty() {
                            self.autoplay_requested = None;
                            self.queue.replace(list, 0);
                            self.play_index(0);
                            self.flash("Radio started", false);
                        }
                    }
                    None => {}
                }
            }
            YtmEvent::Rated { video_id, liked } => {
                self.likes.insert(video_id.clone(), liked);
                for e in &mut self.queue.entries {
                    if e.track.video_id == video_id {
                        e.track.liked = Some(liked);
                    }
                }
                self.flash(if liked { "♥ Liked" } else { "Removed like" }, false);
            }
            YtmEvent::Lyrics { video_id, text } => {
                if let LyricsState::Loading { video_id: v, .. } = &self.lyrics
                    && *v == video_id
                {
                    self.lyrics = match text {
                        Some(t) => LyricsState::Found {
                            lyrics: Lyrics::Plain(t),
                            source: "YouTube Music",
                        },
                        None => LyricsState::Missing,
                    };
                }
            }
        }
    }

    fn on_loaded(&mut self, token: u64, title: String, items: Vec<Item>) {
        match self.pending.remove(&token) {
            Some(Pending::View) => {
                if let Some(lv) = self.find_view(token) {
                    lv.loading = false;
                    if lv.title.is_empty() || !matches!(lv.view, View::Search { .. }) {
                        lv.title = title;
                    }
                    lv.items = items;
                    let first = lv.items.iter().position(|i| !matches!(i, Item::Header(_)));
                    lv.state.select(first);
                    if lv.items.is_empty() {
                        lv.error = Some("Nothing found".into());
                    }
                    // The top search hit is the likeliest next play.
                    if matches!(lv.view, View::Search { .. })
                        && let Some(id) = lv.items.iter().find_map(Item::as_track).map(|t| t.video_id.clone())
                    {
                        self.prefetch(&id);
                    }
                }
            }
            Some(Pending::Enqueue(mode)) => {
                let tracks: Vec<Track> = items.iter().filter_map(Item::as_track).cloned().collect();
                if tracks.is_empty() {
                    self.flash("No playable tracks", true);
                } else {
                    self.enqueue(tracks, mode);
                }
            }
            Some(Pending::PlayFirst) => match items.iter().find_map(Item::as_track) {
                Some(t) => self.enqueue(vec![t.clone()], EnqueueMode::PlayNow),
                None => self.flash("No results", true),
            },
            None => {}
            Some(Pending::LikedSet) => {
                for t in items.iter().filter_map(Item::as_track) {
                    self.likes.insert(t.video_id.clone(), true);
                }
                self.likes_loaded = true;
                self.dirty = true;
            }
        }
    }

    pub fn on_lyrics(&mut self, ev: LyricsEvent) {
        let LyricsState::Loading { video_id, .. } = &self.lyrics else {
            return;
        };
        if *video_id != ev.video_id {
            return;
        }
        self.dirty = true;
        match ev.lyrics {
            Some(lyrics) => {
                self.lyrics = LyricsState::Found {
                    lyrics,
                    source: "LRCLIB",
                }
            }
            None => {
                let _ = self.tx.ytm.send(YtmReq::Lyrics { video_id: ev.video_id });
            }
        }
    }

    pub fn on_art(&mut self, ev: ArtEvent) {
        if self.art_key.as_deref() != Some(&ev.key) {
            return;
        }
        self.art_loading = false;
        self.dirty = true;
        if let (Some(img), Some(picker), Some(art)) = (ev.image, self.picker.as_ref(), self.art.as_mut()) {
            art.replace_protocol(picker.new_resize_protocol(img));
        }
    }

    fn find_view(&mut self, token: u64) -> Option<&mut ListView> {
        self.search
            .stack
            .iter_mut()
            .chain(self.library.stack.iter_mut())
            .find(|lv| lv.token == token)
    }

    fn all_list_tracks(&self) -> impl Iterator<Item = &Track> {
        self.search
            .stack
            .iter()
            .chain(self.library.stack.iter())
            .flat_map(|lv| lv.items.iter().filter_map(Item::as_track))
    }

    fn load_view(&mut self, view: View) -> ListView {
        let token = self.token();
        self.pending.insert(token, Pending::View);
        let _ = self.tx.ytm.send(YtmReq::Load {
            token,
            view: view.clone(),
        });
        ListView::new(view, token)
    }

    /// Search for songs and play the first hit.
    pub fn play_query(&mut self, query: String) {
        let token = self.token();
        self.pending.insert(token, Pending::PlayFirst);
        self.search.input = query.clone();
        self.search.cursor = query.chars().count();
        let _ = self.tx.ytm.send(YtmReq::Load {
            token,
            view: View::Search {
                filter: SearchFilter::Songs,
                query,
            },
        });
        self.flash("Searching…", false);
    }

    fn load_enqueue(&mut self, view: View, mode: EnqueueMode) {
        let token = self.token();
        self.pending.insert(token, Pending::Enqueue(mode));
        let _ = self.tx.ytm.send(YtmReq::Load { token, view });
        self.flash("Loading…", false);
    }

    // ───────────────────────────── input ─────────────────────────────

    pub fn on_terminal(&mut self, ev: Event) {
        match ev {
            Event::Key(k) if k.kind != KeyEventKind::Release => self.on_key(k),
            Event::Mouse(m) => self.on_mouse(m),
            Event::Resize(..) => self.dirty = true,
            Event::Paste(text) if self.search.editing => {
                let before = self.search.input.clone();
                for c in text.chars().filter(|c| !c.is_control()) {
                    self.insert_char(c);
                }
                self.on_input_changed(&before);
                self.dirty = true;
            }
            _ => {}
        }
    }

    fn on_key(&mut self, k: KeyEvent) {
        tracing::debug!("key {:?} {:?} -> {:?}", k.code, k.modifiers, self.keymap.action(&k));
        self.dirty = true;
        if k.code == KeyCode::Char('c') && k.modifiers.contains(KeyModifiers::CONTROL) {
            self.quit = true;
            return;
        }
        // Esc closes an error box first (it's drawn on top of everything).
        if k.code == KeyCode::Esc && self.flash.as_ref().is_some_and(|f| f.error) {
            self.flash = None;
            return;
        }
        if self.overlay.is_some() {
            self.on_overlay_key(k);
            return;
        }
        if self.tab == Tab::Search && self.search.editing && self.on_edit_key(k) {
            return;
        }
        if let Some(action) = self.keymap.action(&k) {
            self.dispatch(action);
        }
    }

    fn insert_char(&mut self, c: char) {
        let byte = self
            .search
            .input
            .char_indices()
            .nth(self.search.cursor)
            .map_or(self.search.input.len(), |(i, _)| i);
        self.search.input.insert(byte, c);
        self.search.cursor += 1;
    }

    /// Text input for the search box. Returns true if the key was consumed.
    fn on_edit_key(&mut self, k: KeyEvent) -> bool {
        let ctrl = k.modifiers.contains(KeyModifiers::CONTROL);
        let n_sugg = self.search.visible_suggestions().len();
        let before = self.search.input.clone();
        let s = &mut self.search;
        match k.code {
            KeyCode::Enter => {
                // Search the highlighted suggestion, like the YTM search bar.
                if let Some(text) = s
                    .suggest_sel
                    .and_then(|i| s.visible_suggestions().get(i))
                    .map(|x| x.text.clone())
                {
                    s.cursor = text.chars().count();
                    s.input = text;
                }
                s.editing = false;
                s.suggest_sel = None;
                self.submit_search();
            }
            KeyCode::Esc if s.suggest_sel.is_some() => s.suggest_sel = None,
            KeyCode::Esc => s.editing = false,
            KeyCode::Down if n_sugg > 0 => {
                s.suggest_sel = Some(s.suggest_sel.map_or(0, |i| (i + 1).min(n_sugg - 1)));
            }
            KeyCode::Up if n_sugg > 0 => s.suggest_sel = s.suggest_sel.and_then(|i| i.checked_sub(1)),
            KeyCode::Down if !s.stack.is_empty() => s.editing = false,
            KeyCode::Backspace if ctrl => {
                s.input.clear();
                s.cursor = 0;
            }
            KeyCode::Char('u') | KeyCode::Char('w') if ctrl => {
                s.input.clear();
                s.cursor = 0;
            }
            KeyCode::Backspace => {
                if s.cursor > 0 {
                    s.cursor -= 1;
                    let byte = s.input.char_indices().nth(s.cursor).map(|(i, _)| i).unwrap();
                    s.input.remove(byte);
                }
            }
            KeyCode::Delete => {
                if let Some((byte, _)) = s.input.char_indices().nth(s.cursor) {
                    s.input.remove(byte);
                }
            }
            KeyCode::Left => s.cursor = s.cursor.saturating_sub(1),
            // → at the end of the line accepts the inline completion.
            KeyCode::Right | KeyCode::End if s.cursor == s.input.chars().count() && s.ghost().is_some() => {
                let full = format!("{}{}", s.input, s.ghost().unwrap_or_default());
                s.cursor = full.chars().count();
                s.input = full;
                s.suggest_sel = None;
            }
            KeyCode::Right => s.cursor = (s.cursor + 1).min(s.input.chars().count()),
            KeyCode::Home => s.cursor = 0,
            KeyCode::End => s.cursor = s.input.chars().count(),
            KeyCode::Tab => self.cycle_filter(true),
            KeyCode::BackTab => self.cycle_filter(false),
            KeyCode::Char(c) if !ctrl => self.insert_char(c),
            _ => return false,
        }
        self.on_input_changed(&before);
        true
    }

    /// Debounce an autocomplete request after the text changed.
    fn on_input_changed(&mut self, before: &str) {
        if self.search.input == before {
            return;
        }
        self.search.suggest_sel = None;
        self.search.suggest_due = if self.search.input.trim().is_empty() {
            None
        } else {
            Some(Instant::now() + Duration::from_millis(180))
        };
    }

    fn cycle_filter(&mut self, forward: bool) {
        let all = SearchFilter::ALL;
        let i = all.iter().position(|f| *f == self.search.filter).unwrap_or(0);
        let n = all.len();
        self.search.filter = all[if forward { (i + 1) % n } else { (i + n - 1) % n }];
        if !self.search.editing && !self.search.input.trim().is_empty() {
            self.submit_search();
        }
    }

    fn submit_search(&mut self) {
        let query = self.search.input.trim().to_string();
        if query.is_empty() {
            return;
        }
        let lv = self.load_view(View::Search {
            filter: self.search.filter,
            query,
        });
        self.search.stack = vec![lv];
    }

    fn switch_library(&mut self, section: LibSection) {
        self.library.section = section;
        let lv = self.load_view(section.view());
        self.library.stack = vec![lv];
    }

    fn set_tab(&mut self, tab: Tab) {
        self.tab = tab;
        match tab {
            Tab::Search => {
                if self.search.stack.is_empty() {
                    self.search.editing = true;
                }
            }
            Tab::Library => {
                if self.library.stack.is_empty() {
                    self.switch_library(self.library.section);
                }
            }
            Tab::Queue if self.queue_state.selected().is_none() => {
                self.queue_state.select(self.queue.current);
            }
            _ => {}
        }
    }

    /// The list the navigation keys currently act on.
    fn active_list(&mut self) -> Option<(&mut TableState, usize)> {
        match self.tab {
            Tab::Queue => {
                let n = self.queue.len();
                Some((&mut self.queue_state, n))
            }
            Tab::Search => self.search.stack.last_mut().map(|lv| {
                let n = lv.items.len();
                (&mut lv.state, n)
            }),
            Tab::Library => self.library.stack.last_mut().map(|lv| {
                let n = lv.items.len();
                (&mut lv.state, n)
            }),
            _ => None,
        }
    }

    fn move_selection(&mut self, delta: isize) {
        if self.tab == Tab::Lyrics {
            let cur = self.lyrics_scroll.unwrap_or(self.lyrics_auto) as isize;
            self.lyrics_scroll = Some((cur + delta).max(0) as u16);
            return;
        }
        // Skip section headers in browse lists.
        let headers: Vec<bool> = match self.tab {
            Tab::Search => self
                .search
                .stack
                .last()
                .map(|l| l.items.iter().map(|i| matches!(i, Item::Header(_))).collect()),
            Tab::Library => self
                .library
                .stack
                .last()
                .map(|l| l.items.iter().map(|i| matches!(i, Item::Header(_))).collect()),
            _ => None,
        }
        .unwrap_or_default();
        let Some((state, len)) = self.active_list() else { return };
        if len == 0 {
            return;
        }
        let cur = state.selected().unwrap_or(0) as isize;
        let mut next = (cur + delta).clamp(0, len as isize - 1) as usize;
        let dir = if delta >= 0 { 1isize } else { -1 };
        while headers.get(next).copied().unwrap_or(false) {
            let n = next as isize + dir;
            if n < 0 || n >= len as isize {
                // Nowhere to go in that direction; search the other way.
                next = (0..len)
                    .find(|&i| !headers.get(i).copied().unwrap_or(false))
                    .unwrap_or(0);
                break;
            }
            next = n as usize;
        }
        state.select(Some(next));
    }

    pub fn dispatch(&mut self, action: Action) {
        use Action::*;
        self.dirty = true;
        match action {
            Quit => self.quit = true,
            PlayPause => {
                if self.player.idle && self.load_started.is_none() {
                    match self
                        .queue
                        .current
                        .or(if self.queue.is_empty() { None } else { Some(0) })
                    {
                        Some(i) => self.play_index(i),
                        None => self.flash("Queue is empty — press / to search", false),
                    }
                } else {
                    self.player(PlayerCmd::TogglePause);
                }
            }
            SeekForward => self.player(PlayerCmd::SeekRelative(5.0)),
            SeekBackward => self.player(PlayerCmd::SeekRelative(-5.0)),
            SeekForwardBig => self.player(PlayerCmd::SeekRelative(30.0)),
            SeekBackwardBig => self.player(PlayerCmd::SeekRelative(-30.0)),
            Next => self.next_track(),
            Prev => self.prev_track(),
            VolumeUp | VolumeDown => {
                let d = if action == VolumeUp { 5.0 } else { -5.0 };
                self.player.volume = ((self.player.volume / 5.0).round() * 5.0 + d).clamp(0.0, 100.0);
                self.player(PlayerCmd::SetVolume(self.player.volume));
                if self.player.muted {
                    self.player(PlayerCmd::SetMute(false));
                }
            }
            Mute => self.player(PlayerCmd::SetMute(!self.player.muted)),
            Shuffle => {
                let on = !self.queue.shuffle;
                self.queue.set_shuffle(on);
                self.sync_next();
                self.flash(if on { "Shuffle on" } else { "Shuffle off" }, false);
            }
            Repeat => self.set_repeat(self.queue.repeat.next()),
            Like => {
                let Some(t) = self.current() else { return };
                if !self.logged_in {
                    self.flash(
                        "Liking songs needs you to be signed in. Run `pova-import-cookie chrome` \
                         (or firefox) in another terminal; pova signs in by itself once it's saved.",
                        true,
                    );
                    return;
                }
                let known = self.likes_loaded || self.likes.contains_key(&t.video_id) || t.liked.is_some();
                if !known {
                    self.flash("Still loading your liked songs — try again in a moment", false);
                    return;
                }
                let liked = !is_liked(&self.likes, t);
                let _ = self.tx.ytm.send(YtmReq::Rate {
                    video_id: t.video_id.clone(),
                    liked,
                });
            }
            VizMode => {
                self.viz_mode = self.viz_mode.next();
                self.flash(format!("Visualizer: {}", self.viz_mode.label()), false);
            }
            Themes => self.open_themes(),
            Help => self.overlay = Some(Overlay::Help { scroll: 0 }),
            Menu => self.back_or_menu(),
            TabNext | TabPrev => {
                let i = Tab::ALL.iter().position(|t| *t == self.tab).unwrap_or(0);
                let n = Tab::ALL.len();
                self.set_tab(
                    Tab::ALL[if action == TabNext {
                        (i + 1) % n
                    } else {
                        (i + n - 1) % n
                    }],
                );
            }
            TabNowPlaying => self.set_tab(Tab::NowPlaying),
            TabQueue => self.set_tab(Tab::Queue),
            TabLyrics => self.set_tab(Tab::Lyrics),
            TabSearch => {
                self.set_tab(Tab::Search);
                self.search.editing = true;
                self.search.cursor = self.search.input.chars().count();
            }
            TabLibrary => self.set_tab(Tab::Library),
            ToggleViz => self.show_viz = !self.show_viz,
            TogglePlayer => self.show_player = !self.show_player,
            ToggleControls => self.show_controls = !self.show_controls,
            Up => self.move_selection(-1),
            Down => self.move_selection(1),
            PageUp => self.move_selection(-10),
            PageDown => self.move_selection(10),
            Top => self.move_selection(-100_000),
            Bottom => self.move_selection(100_000),
            Select => self.select(EnqueueMode::PlayNow),
            Append => self.select(EnqueueMode::Append),
            PlayNext => self.select(EnqueueMode::Next),
            Remove => self.remove_selected(),
            MoveUp | MoveDown => {
                if self.tab == Tab::Queue
                    && let Some(i) = self.queue_state.selected()
                    && let Some(j) = self.queue.move_item(i, action == MoveUp)
                {
                    self.queue_state.select(Some(j));
                    self.sync_next();
                }
            }
            Radio => {
                let seed = match self.tab {
                    Tab::Queue => self.queue_state.selected().and_then(|i| self.queue.get(i)).cloned(),
                    Tab::Search => self
                        .search
                        .stack
                        .last()
                        .and_then(|l| l.selected())
                        .and_then(Item::as_track)
                        .cloned(),
                    Tab::Library => self
                        .library
                        .stack
                        .last()
                        .and_then(|l| l.selected())
                        .and_then(Item::as_track)
                        .cloned(),
                    _ => self.current().cloned(),
                };
                if let Some(t) = seed {
                    self.radio.insert(t.video_id.clone(), RadioMode::Start);
                    let _ = self.tx.ytm.send(YtmReq::Radio { seed: t.video_id });
                    self.flash(format!("Starting radio from {}…", t.title), false);
                }
            }
            SubPrev | SubNext => {
                let fwd = action == SubNext;
                match self.tab {
                    Tab::Search => self.cycle_filter(fwd),
                    Tab::Library => {
                        let all = LibSection::ALL;
                        let i = all.iter().position(|s| *s == self.library.section).unwrap_or(0);
                        let n = all.len();
                        self.switch_library(all[if fwd { (i + 1) % n } else { (i + n - 1) % n }]);
                    }
                    _ => {}
                }
            }
            ClearQueue => {
                self.queue.clear_upcoming();
                self.queue_state.select(self.queue.current);
                self.sync_next();
                self.flash("Queue cleared", false);
            }
            Refresh => match self.tab {
                Tab::Search => {
                    if let Some(view) = self.search.stack.last().map(|l| l.view.clone()) {
                        let lv = self.load_view(view);
                        *self.search.stack.last_mut().unwrap() = lv;
                    }
                }
                Tab::Library => {
                    if let Some(view) = self.library.stack.last().map(|l| l.view.clone()) {
                        let lv = self.load_view(view);
                        *self.library.stack.last_mut().unwrap() = lv;
                    }
                }
                _ => {}
            },
            JumpToCurrent => match self.tab {
                Tab::Lyrics => self.lyrics_scroll = None,
                _ => {
                    self.set_tab(Tab::Queue);
                    self.queue_state.select(self.queue.current);
                }
            },
        }
    }

    fn back_or_menu(&mut self) {
        match self.tab {
            Tab::Search if self.search.stack.len() > 1 => {
                self.search.stack.pop();
            }
            Tab::Library if self.library.stack.len() > 1 => {
                self.library.stack.pop();
            }
            Tab::Lyrics if self.lyrics_scroll.is_some() => self.lyrics_scroll = None,
            _ => {
                let mut state = TableState::default();
                state.select(Some(0));
                self.overlay = Some(Overlay::Menu { state });
            }
        }
    }

    fn select(&mut self, mode: EnqueueMode) {
        match self.tab {
            Tab::Queue => {
                if let Some(i) = self.queue_state.selected() {
                    match mode {
                        EnqueueMode::PlayNow => self.play_index(i),
                        _ => {
                            if let Some(t) = self.queue.get(i).cloned() {
                                self.enqueue(vec![t], mode);
                            }
                        }
                    }
                }
            }
            Tab::Search | Tab::Library => self.select_in_browser(mode),
            Tab::NowPlaying | Tab::Lyrics => {
                if mode == EnqueueMode::PlayNow {
                    self.dispatch(Action::PlayPause);
                }
            }
        }
    }

    fn select_in_browser(&mut self, mode: EnqueueMode) {
        let stack = if self.tab == Tab::Search {
            &self.search.stack
        } else {
            &self.library.stack
        };
        let Some(lv) = stack.last() else { return };
        let Some(item) = lv.selected().cloned() else { return };
        let view = lv.view.clone();
        let whole_list = matches!(view, View::Album(_) | View::Playlist(_) | View::LikedSongs);
        let list_tracks = lv.tracks();
        match item {
            Item::Track(t) => {
                if mode == EnqueueMode::PlayNow && whole_list {
                    // Play the album/playlist from this track onwards.
                    let start = list_tracks.iter().position(|x| x.video_id == t.video_id).unwrap_or(0);
                    self.autoplay_requested = None;
                    self.queue.replace(list_tracks, start);
                    self.play_index(self.queue.current.unwrap_or(0));
                } else {
                    self.enqueue(vec![t], mode);
                }
            }
            Item::Album(a) => self.open_or_enqueue(View::Album(a.id), mode),
            Item::Playlist(p) => self.open_or_enqueue(View::Playlist(p.id), mode),
            Item::Artist(a) => {
                if mode == EnqueueMode::PlayNow {
                    self.push_view(View::Artist(a.id));
                } else {
                    self.flash("Open the artist to pick songs", false);
                }
            }
            Item::Header(_) => {}
        }
    }

    fn open_or_enqueue(&mut self, view: View, mode: EnqueueMode) {
        if mode == EnqueueMode::PlayNow {
            self.push_view(view);
        } else {
            self.load_enqueue(view, mode);
        }
    }

    fn push_view(&mut self, view: View) {
        let lv = self.load_view(view);
        if self.tab == Tab::Search {
            self.search.stack.push(lv);
        } else {
            self.library.stack.push(lv);
        }
    }

    fn remove_selected(&mut self) {
        if self.tab != Tab::Queue {
            return;
        }
        let Some(i) = self.queue_state.selected() else { return };
        let was_current = self.queue.current == Some(i);
        if self.queue.remove(i).is_none() {
            return;
        }
        if self.queue.is_empty() {
            self.queue_state.select(None);
            self.player(PlayerCmd::Stop);
            self.next_sent = None;
            return;
        }
        self.queue_state.select(Some(i.min(self.queue.len() - 1)));
        if was_current {
            if let Some(c) = self.queue.current {
                self.play_index(c);
            }
        } else {
            self.sync_next();
        }
    }

    // ───────────────────────────── overlays ─────────────────────────────

    fn open_themes(&mut self) {
        let names = theme::list_themes(&self.paths.config_dir);
        let mut state = TableState::default();
        state.select(Some(names.iter().position(|(n, _)| *n == self.theme.name).unwrap_or(0)));
        self.overlay = Some(Overlay::Themes {
            names,
            state,
            original: self.theme.clone(),
        });
    }

    fn preview_theme(&mut self) {
        if let Some(Overlay::Themes { names, state, .. }) = &self.overlay
            && let Some((name, path)) = state.selected().and_then(|i| names.get(i))
        {
            let bg = self.cfg.theme_background;
            self.theme = match path {
                Some(p) => Theme::load(p, bg).unwrap_or_else(|_| Theme::builtin_default(bg)),
                None => Theme::builtin_default(bg),
            };
            if name != &self.theme.name {
                self.theme.name = name.clone();
            }
        }
    }

    fn on_overlay_key(&mut self, k: KeyEvent) {
        let action = self.keymap.action(&k);
        let Some(overlay) = self.overlay.as_mut() else { return };
        match overlay {
            Overlay::Help { scroll } => match action {
                Some(Action::Down) => *scroll += 1,
                Some(Action::Up) => *scroll = scroll.saturating_sub(1),
                Some(Action::PageDown) => *scroll += 10,
                Some(Action::PageUp) => *scroll = scroll.saturating_sub(10),
                Some(Action::Quit) => self.quit = true,
                _ => self.overlay = None,
            },
            Overlay::Themes { names, state, original } => {
                let len = names.len();
                let cur = state.selected().unwrap_or(0);
                match (k.code, action) {
                    (KeyCode::Enter, _) => {
                        self.overlay = None;
                        self.save_state();
                        self.flash(format!("Theme: {}", self.theme.name), false);
                    }
                    (KeyCode::Esc, _) | (_, Some(Action::Themes)) => {
                        self.theme = original.clone();
                        self.overlay = None;
                    }
                    (_, Some(Action::Down)) => {
                        state.select(Some((cur + 1) % len));
                        self.preview_theme();
                    }
                    (_, Some(Action::Up)) => {
                        state.select(Some((cur + len - 1) % len));
                        self.preview_theme();
                    }
                    (_, Some(Action::PageDown)) => {
                        state.select(Some((cur + 10).min(len - 1)));
                        self.preview_theme();
                    }
                    (_, Some(Action::PageUp)) => {
                        state.select(Some(cur.saturating_sub(10)));
                        self.preview_theme();
                    }
                    _ => {}
                }
            }
            Overlay::Menu { state } => {
                let len = MenuItem::ALL.len();
                let cur = state.selected().unwrap_or(0);
                let item = MenuItem::ALL[cur.min(len - 1)];
                let is_setting = matches!(item, MenuItem::VizMode | MenuItem::Autoplay | MenuItem::AlbumArt);
                match (k.code, action) {
                    // Settings toggle in place (menu stays open, like btop's options).
                    (KeyCode::Enter | KeyCode::Left | KeyCode::Right, _) if is_setting => self.activate_menu(item),
                    (KeyCode::Enter, _) | (_, Some(Action::Select)) => {
                        self.overlay = None;
                        self.activate_menu(item);
                    }
                    (KeyCode::Esc, _) => self.overlay = None,
                    (_, Some(Action::Down)) => state.select(Some((cur + 1) % len)),
                    (_, Some(Action::Up)) => state.select(Some((cur + len - 1) % len)),
                    (_, Some(Action::Quit)) => self.quit = true,
                    _ => {}
                }
            }
        }
    }

    // ───────────────────────────── mouse ─────────────────────────────

    fn on_mouse(&mut self, m: MouseEvent) {
        match m.kind {
            MouseEventKind::ScrollDown => {
                if self.overlay.is_none() {
                    self.move_selection(3);
                }
            }
            MouseEventKind::ScrollUp => {
                if self.overlay.is_none() {
                    self.move_selection(-3);
                }
            }
            MouseEventKind::Down(MouseButton::Left) => self.on_click(m.column, m.row),
            _ => return,
        }
        self.dirty = true;
    }

    fn on_click(&mut self, x: u16, y: u16) {
        if self.overlay.is_some() {
            self.overlay = None;
            return;
        }
        let double = self
            .last_click
            .is_some_and(|(t, lx, ly)| t.elapsed() < Duration::from_millis(400) && lx == x && ly == y);
        self.last_click = Some((Instant::now(), x, y));

        if let Some((rect, hit)) = self.hits.at(x, y) {
            match hit {
                Hit::Tab(t) => self.set_tab(t),
                Hit::Action(a) => self.dispatch(a),
                Hit::Progress => {
                    if self.player.duration > 0.0 && rect.width > 1 {
                        let frac = (x - rect.x) as f64 / (rect.width - 1) as f64;
                        self.player(PlayerCmd::SeekAbsolute(frac * self.player.duration));
                    }
                }
                Hit::SearchFilter(f) => {
                    self.search.filter = f;
                    if !self.search.input.trim().is_empty() {
                        self.submit_search();
                    }
                }
                Hit::LibSection(s) => self.switch_library(s),
                Hit::CloseError => {
                    self.flash = None;
                    self.dirty = true;
                }
                Hit::Suggestion(i) => {
                    if let Some(text) = self.search.visible_suggestions().get(i).map(|x| x.text.clone()) {
                        self.search.cursor = text.chars().count();
                        self.search.input = text;
                        self.search.editing = false;
                        self.search.suggest_sel = None;
                        self.submit_search();
                    }
                }
                Hit::List => {
                    if let Some((area, offset)) = self.hits.list {
                        let idx = offset + (y - area.y) as usize;
                        if self.tab == Tab::Search {
                            self.search.editing = false;
                        }
                        if let Some((state, len)) = self.active_list()
                            && idx < len
                        {
                            let already = state.selected() == Some(idx);
                            state.select(Some(idx));
                            if already || double {
                                self.select(EnqueueMode::PlayNow);
                            }
                        }
                    }
                }
            }
        }
    }
}

#[cfg(test)]
pub mod tests_support {
    use super::*;

    pub fn list_view() -> ListView {
        ListView::new(
            View::Search {
                filter: SearchFilter::Songs,
                query: "q".into(),
            },
            1,
        )
    }
}

#[cfg(test)]
mod search_tests {
    use super::*;
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
    use tokio::sync::mpsc::{UnboundedReceiver, unbounded_channel};

    fn app() -> (App, UnboundedReceiver<YtmReq>) {
        let (p, _) = unbounded_channel();
        let (y, yrx) = unbounded_channel();
        let (l, _) = unbounded_channel();
        let (a, _) = unbounded_channel();
        let tmp = std::env::temp_dir().join("pova-search-test");
        let paths = Paths {
            config_dir: tmp.clone(),
            cache_dir: tmp.clone(),
            state_dir: tmp.clone(),
            runtime_dir: tmp,
        };
        let (keymap, _) = Keymap::new(&Default::default());
        let app = App::new(
            Config::default(),
            paths,
            State::default(),
            keymap,
            Channels {
                player: p,
                ytm: y,
                lyrics: l,
                art: a,
                resolve: unbounded_channel().0,
            },
            None,
            None,
        );
        (app, yrx)
    }

    fn key(app: &mut App, code: KeyCode) {
        app.on_terminal(Event::Key(KeyEvent::new(code, KeyModifiers::NONE)));
    }

    fn sugg(text: &str) -> Suggestion {
        Suggestion {
            text: text.into(),
            history: false,
        }
    }

    #[test]
    fn typing_debounces_then_requests_suggestions() {
        let (mut app, mut rx) = app();
        app.dispatch(Action::TabSearch);
        for c in "call me".chars() {
            key(&mut app, KeyCode::Char(c));
        }
        assert!(rx.try_recv().is_err(), "no request before the debounce");
        app.search.suggest_due = Some(Instant::now());
        app.tick();
        assert!(matches!(rx.try_recv(), Ok(YtmReq::Suggest { query }) if query == "call me"));
    }

    #[test]
    fn stale_suggestions_ignored_and_ghost_completion() {
        let (mut app, _rx) = app();
        app.dispatch(Action::TabSearch);
        for c in "Call Me".chars() {
            key(&mut app, KeyCode::Char(c));
        }
        app.on_ytm(YtmEvent::Suggestions {
            query: "cal".into(),
            items: vec![sugg("calvin harris")],
        });
        assert!(app.search.visible_suggestions().is_empty());
        app.on_ytm(YtmEvent::Suggestions {
            query: "Call Me".into(),
            items: vec![sugg("call me when you're sober"), sugg("call me maybe")],
        });
        // Case-insensitive inline completion; → accepts it.
        assert_eq!(app.search.ghost(), Some(" when you're sober"));
        key(&mut app, KeyCode::Right);
        assert_eq!(app.search.input, "Call Me when you're sober");
    }

    #[test]
    fn down_enter_searches_selected_suggestion() {
        let (mut app, mut rx) = app();
        app.dispatch(Action::TabSearch);
        for c in "call me".chars() {
            key(&mut app, KeyCode::Char(c));
        }
        app.on_ytm(YtmEvent::Suggestions {
            query: "call me".into(),
            items: vec![sugg("call me when you're sober"), sugg("call me maybe")],
        });
        key(&mut app, KeyCode::Down);
        key(&mut app, KeyCode::Down);
        assert_eq!(app.search.suggest_sel, Some(1));
        key(&mut app, KeyCode::Enter);
        assert!(!app.search.editing);
        assert_eq!(app.search.input, "call me maybe");
        assert!(matches!(
            rx.try_recv(),
            Ok(YtmReq::Load { view: View::Search { query, .. }, .. }) if query == "call me maybe"
        ));
    }

    fn track(id: &str) -> Track {
        Track {
            video_id: id.into(),
            title: id.into(),
            artists: String::new(),
            album: None,
            album_id: None,
            duration: None,
            thumbnail: None,
            liked: None,
        }
    }

    /// Sign in and answer the Liked-songs request with `liked_ids`.
    fn signed_in(liked_ids: &[&str]) -> (App, UnboundedReceiver<YtmReq>) {
        let (mut app, mut rx) = app();
        app.on_ytm(YtmEvent::Auth {
            logged_in: true,
            message: None,
        });
        let token = match rx.try_recv() {
            Ok(YtmReq::Load {
                token,
                view: View::LikedSongs,
            }) => token,
            other => panic!("expected liked-songs load, got {other:?}"),
        };
        assert!(!app.likes_loaded);
        let items = liked_ids.iter().map(|id| Item::Track(track(id))).collect();
        app.on_ytm(YtmEvent::Loaded {
            token,
            title: "Liked songs".into(),
            items,
        });
        assert!(app.likes_loaded);
        (app, rx)
    }

    #[test]
    fn already_liked_song_is_unliked_not_liked_again() {
        let (mut app, mut rx) = signed_in(&["a"]);
        // Search results carry no like status (liked: None).
        app.queue.replace(vec![track("a"), track("b")], 0);
        assert!(is_liked(&app.likes, app.current().unwrap()));
        app.dispatch(Action::Like);
        assert!(matches!(rx.try_recv(), Ok(YtmReq::Rate { video_id, liked: false }) if video_id == "a"));
        app.on_ytm(YtmEvent::Rated {
            video_id: "a".into(),
            liked: false,
        });
        assert!(!is_liked(&app.likes, app.current().unwrap()));
        // Not in Liked songs -> `f` likes it.
        app.queue.current = Some(1);
        app.dispatch(Action::Like);
        assert!(matches!(rx.try_recv(), Ok(YtmReq::Rate { video_id, liked: true }) if video_id == "b"));
    }

    #[test]
    fn like_waits_until_status_known() {
        let (mut app, mut rx) = app();
        app.on_ytm(YtmEvent::Auth {
            logged_in: true,
            message: None,
        });
        let token = match rx.try_recv() {
            Ok(YtmReq::Load { token, .. }) => token,
            other => panic!("{other:?}"),
        };
        app.queue.replace(vec![track("a")], 0);
        app.dispatch(Action::Like);
        assert!(rx.try_recv().is_err(), "must not guess while likes are loading");
        // A failed load must not block liking forever.
        app.on_ytm(YtmEvent::Failed {
            token: Some(token),
            message: "boom".into(),
        });
        assert!(app.likes_loaded);
        app.dispatch(Action::Like);
        assert!(matches!(rx.try_recv(), Ok(YtmReq::Rate { liked: true, .. })));
    }

    #[test]
    fn lists_with_like_status_teach_the_cache() {
        let (mut app, _rx) = signed_in(&[]);
        let mut t = track("x");
        t.liked = Some(true);
        app.on_ytm(YtmEvent::Loaded {
            token: 999,
            title: String::new(),
            items: vec![Item::Track(t)],
        });
        assert_eq!(app.likes.get("x"), Some(&true));
    }
}

#[cfg(test)]
mod resolve_tests {
    use super::*;
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
    use tokio::sync::mpsc::{UnboundedReceiver, unbounded_channel};

    struct Harness {
        app: App,
        player: UnboundedReceiver<PlayerCmd>,
        resolve: UnboundedReceiver<ResolveReq>,
    }

    fn harness() -> Harness {
        let (p, prx) = unbounded_channel();
        let (r, rrx) = unbounded_channel();
        let tmp = std::env::temp_dir().join(format!("pova-resolve-test-{}", std::process::id()));
        std::fs::create_dir_all(&tmp).unwrap();
        let paths = Paths {
            config_dir: tmp.clone(),
            cache_dir: tmp.clone(),
            state_dir: tmp.clone(),
            runtime_dir: tmp,
        };
        let (keymap, _) = Keymap::new(&Default::default());
        let channels = Channels {
            player: p,
            ytm: unbounded_channel().0,
            lyrics: unbounded_channel().0,
            art: unbounded_channel().0,
            resolve: r,
        };
        let app = App::new(Config::default(), paths, State::default(), keymap, channels, None, None);
        Harness {
            app,
            player: prx,
            resolve: rrx,
        }
    }

    fn track(id: &str) -> Track {
        Track {
            video_id: id.into(),
            title: id.into(),
            artists: String::new(),
            album: None,
            album_id: None,
            duration: None,
            thumbnail: None,
            liked: None,
        }
    }

    fn drain<T>(rx: &mut UnboundedReceiver<T>) -> Vec<T> {
        std::iter::from_fn(|| rx.try_recv().ok()).collect()
    }

    fn direct(id: &str) -> String {
        format!("https://rr1.googlevideo.com/videoplayback?id={id}&expire=4102444800")
    }

    fn resolved(h: &mut Harness, id: &str) {
        let url = direct(id);
        let until = crate::resolve::expiry(&url, SystemTime::now());
        h.app.on_resolved(Resolved {
            video_id: id.into(),
            stream: Some((url, until)),
        });
    }

    #[test]
    fn playing_prefetches_next_two() {
        let mut h = harness();
        h.app
            .queue
            .replace(vec![track("a"), track("b"), track("c"), track("d")], 0);
        h.app.play_index(0);
        let ids: Vec<String> = drain(&mut h.resolve).into_iter().map(|r| r.video_id).collect();
        assert_eq!(ids, ["b", "c"]);
    }

    #[test]
    fn resolved_urls_are_used_for_play_and_next() {
        let mut h = harness();
        h.app.queue.replace(vec![track("a"), track("b")], 0);
        resolved(&mut h, "a");
        resolved(&mut h, "b");
        h.app.play_index(0);
        let cmds = drain(&mut h.player);
        assert!(
            cmds.iter().any(
                |c| matches!(c, PlayerCmd::Load { url, next: Some(n) } if *url == direct("a") && *n == direct("b"))
            ),
            "{cmds:?}"
        );
        // mpv reports the direct URL as its path; gapless advance must still map to "b".
        h.app.on_player(PlayerEvent::Path(Some(direct("a"))));
        assert_eq!(h.app.queue.current, Some(0));
        h.app.on_player(PlayerEvent::Path(Some(direct("b"))));
        assert_eq!(h.app.queue.current, Some(1));
    }

    #[test]
    fn late_resolution_upgrades_preloaded_next() {
        let mut h = harness();
        h.app.queue.replace(vec![track("a"), track("b")], 0);
        h.app.play_index(0);
        // mpv starts playing "a" (via the yt-dlp hook) and leaves idle.
        h.app.on_player(PlayerEvent::Idle(false));
        h.app.on_player(PlayerEvent::Path(Some(track("a").url())));
        drain(&mut h.player);
        resolved(&mut h, "b");
        let cmds = drain(&mut h.player);
        assert!(
            cmds.iter()
                .any(|c| matches!(c, PlayerCmd::SetNext(Some(u)) if *u == direct("b"))),
            "{cmds:?}"
        );
    }

    #[test]
    fn failed_direct_url_retries_once_via_ytdlp() {
        let mut h = harness();
        h.app.queue.replace(vec![track("a"), track("b")], 0);
        resolved(&mut h, "a");
        h.app.play_index(0);
        drain(&mut h.player);
        h.app.on_player(PlayerEvent::EndFile {
            reason: "error".into(),
            error: Some("403".into()),
        });
        let cmds = drain(&mut h.player);
        assert!(
            cmds.iter()
                .any(|c| matches!(c, PlayerCmd::Load { url, .. } if *url == track("a").url())),
            "{cmds:?}"
        );
        assert_eq!(h.app.queue.current, Some(0));
    }

    #[test]
    fn expired_streams_are_not_used() {
        let mut h = harness();
        h.app.queue.replace(vec![track("a")], 0);
        h.app.on_resolved(Resolved {
            video_id: "a".into(),
            stream: Some((direct("a"), SystemTime::now() - Duration::from_secs(1))),
        });
        h.app.play_index(0);
        let cmds = drain(&mut h.player);
        assert!(
            cmds.iter()
                .any(|c| matches!(c, PlayerCmd::Load { url, .. } if *url == track("a").url()))
        );
    }

    #[test]
    fn dwelling_on_a_queue_row_prefetches_it() {
        let mut h = harness();
        h.app
            .queue
            .replace(vec![track("a"), track("b"), track("c"), track("d"), track("e")], 0);
        h.app.tab = Tab::Queue;
        h.app.queue_state.select(Some(4));
        h.app.tick();
        assert!(drain(&mut h.resolve).is_empty(), "no prefetch before the dwell");
        h.app.dwell = Some(("e".into(), Instant::now() - Duration::from_secs(1)));
        h.app.tick();
        let ids: Vec<String> = drain(&mut h.resolve).into_iter().map(|r| r.video_id).collect();
        assert_eq!(ids, ["e"]);
        h.app.tick();
        assert!(drain(&mut h.resolve).is_empty(), "only once per selection");
    }

    #[test]
    fn menu_settings_toggle_in_place_and_persist() {
        let mut h = harness();
        h.app.dispatch(Action::Menu); // no list to go back in -> opens the menu
        assert!(matches!(h.app.overlay, Some(Overlay::Menu { .. })));
        let autoplay = MenuItem::ALL.iter().position(|i| *i == MenuItem::Autoplay).unwrap();
        if let Some(Overlay::Menu { state }) = h.app.overlay.as_mut() {
            state.select(Some(autoplay));
        }
        let before = h.app.cfg.autoplay;
        h.app
            .on_terminal(Event::Key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE)));
        assert_eq!(h.app.cfg.autoplay, !before);
        assert!(
            matches!(h.app.overlay, Some(Overlay::Menu { .. })),
            "menu stays open for settings"
        );
        let saved = State::load(&h.app.paths.state_file());
        assert_eq!(saved.autoplay, Some(!before));
        // Themes entry opens the theme picker.
        if let Some(Overlay::Menu { state }) = h.app.overlay.as_mut() {
            state.select(Some(0));
        }
        h.app
            .on_terminal(Event::Key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE)));
        assert!(matches!(h.app.overlay, Some(Overlay::Themes { .. })));
    }
}

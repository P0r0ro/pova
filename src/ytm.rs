//! YouTube Music actor (ytmapi-rs). Every request runs as its own task and
//! reports back through [`YtmEvent`]s.

use std::path::PathBuf;

use futures::StreamExt;
use tokio::sync::mpsc::{UnboundedReceiver, UnboundedSender};
use ytmapi_rs::YtMusic;
use ytmapi_rs::auth::BrowserToken;
use ytmapi_rs::auth::noauth::NoAuthToken;
use ytmapi_rs::common::{AlbumID, ArtistChannelID, LikeStatus, PlaylistID, Thumbnail, VideoID, YoutubeID};
use ytmapi_rs::parse::{HistoryItem, PlaylistItem, SearchResultPlaylist, SearchResultVideo};
use ytmapi_rs::query::GetPlaylistTracksQuery;

use crate::model::{AlbumRef, ArtistRef, Item, PlaylistRef, Track, parse_duration};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum SearchFilter {
    #[default]
    Songs,
    Videos,
    Albums,
    Artists,
    Playlists,
}

impl SearchFilter {
    pub const ALL: [SearchFilter; 5] = [
        SearchFilter::Songs,
        SearchFilter::Videos,
        SearchFilter::Albums,
        SearchFilter::Artists,
        SearchFilter::Playlists,
    ];
    pub fn label(self) -> &'static str {
        match self {
            SearchFilter::Songs => "Songs",
            SearchFilter::Videos => "Videos",
            SearchFilter::Albums => "Albums",
            SearchFilter::Artists => "Artists",
            SearchFilter::Playlists => "Playlists",
        }
    }
}

/// Something that can be loaded into a list view.
#[derive(Clone, Debug, PartialEq)]
pub enum View {
    Search { filter: SearchFilter, query: String },
    Album(String),
    Artist(String),
    Playlist(String),
    LibraryPlaylists,
    LikedSongs,
    History,
}

#[derive(Debug)]
pub enum YtmReq {
    Load {
        token: u64,
        view: View,
    },
    Radio {
        seed: String,
    },
    Rate {
        video_id: String,
        liked: bool,
    },
    /// Thumbs-down: YouTube plays it less and steers radio away from it.
    Dislike {
        video_id: String,
    },
    Lyrics {
        video_id: String,
    },
    /// Autocomplete suggestions for a partially typed query.
    Suggest {
        query: String,
    },
    AddToPlaylist {
        playlist_id: String,
        playlist_title: String,
        video_id: String,
        title: String,
    },
    /// New private playlist containing one song.
    CreatePlaylist {
        name: String,
        video_id: String,
        title: String,
    },
    RemoveFromPlaylist {
        playlist_id: String,
        video_id: String,
        title: String,
    },
    DeletePlaylist {
        playlist_id: String,
        title: String,
    },
    /// Auto-detect browsers, sign in immediately if already logged in, or
    /// open `https://music.youtube.com` and wait for the user to sign in.
    StartSignIn,
    /// Import YouTube Music cookies from a specific browser/profile spec.
    ImportBrowser {
        spec: String,
        label: String,
    },
    /// Quietly check whether the user has finished signing in across `specs`.
    PollSignIn {
        specs: Vec<String>,
    },
    /// Open `https://music.youtube.com` in the user's browser.
    OpenSignInLink {
        spec: Option<String>,
    },
    /// Save a manually pasted Cookie header and sign in.
    SaveCookie {
        raw: String,
    },
    /// Remove the saved cookie and drop back to signed-out mode.
    SignOut,
}

/// One autocomplete entry.
#[derive(Clone, Debug, PartialEq)]
pub struct Suggestion {
    pub text: String,
    /// From the signed-in user's own search history.
    pub history: bool,
}

#[derive(Debug)]
pub enum YtmEvent {
    Suggestions {
        query: String,
        items: Vec<Suggestion>,
    },
    Auth {
        logged_in: bool,
        message: Option<String>,
    },
    /// No active browser login was found yet; we opened `https://music.youtube.com`
    /// and will watch `specs` while the user signs in with Google.
    AuthNeedsBrowser {
        specs: Vec<String>,
        browser_opened: bool,
    },
    /// A background poll while waiting for browser sign-in didn't find a login yet.
    AuthPollPending,
    Loaded {
        token: u64,
        title: String,
        items: Vec<Item>,
    },
    Radio {
        seed: String,
        tracks: Vec<Track>,
    },
    Rated {
        video_id: String,
        liked: bool,
    },
    Disliked {
        video_id: String,
    },
    Lyrics {
        video_id: String,
        text: Option<String>,
    },
    Failed {
        token: Option<u64>,
        message: String,
    },
    /// A playlist edit worked. `removed` = (playlist id, video id).
    PlaylistEdited {
        message: String,
        removed: Option<(String, String)>,
    },
    PlaylistDeleted {
        playlist_id: String,
        title: String,
    },
}

#[derive(Clone)]
enum Client {
    Anon(YtMusic<NoAuthToken>),
    Auth(YtMusic<BrowserToken>),
}

/// Run `$body` with `$yt` bound to whichever client variant we have.
macro_rules! with_yt {
    ($client:expr, $yt:ident => $body:expr) => {
        match $client {
            Client::Anon($yt) => $body,
            Client::Auth($yt) => $body,
        }
    };
}

/// Max continuation pages fetched for long playlists (~100 tracks each).
const MAX_PAGES: usize = 20;

/// Shown when YouTube Music rejects the saved cookie.
pub const SIGNED_OUT: &str = "YouTube Music signed you out: the saved login cookie has expired. \
Open the Library tab (b) to sign in again, or run `pova login`.";

enum AuthOutcome {
    SignedIn {
        client: YtMusic<BrowserToken>,
    },
    NeedsBrowser {
        specs: Vec<String>,
        browser_opened: bool,
    },
    PollPending,
    Error {
        message: String,
    },
}

/// Check every detected browser for an existing YouTube Music login; if none
/// is signed in yet, open `https://music.youtube.com` and return the readable
/// browsers to watch while the user finishes signing in.
async fn start_auto_sign_in(cookie_file: PathBuf) -> AuthOutcome {
    use crate::auth::{ExtractError, detect_browsers, extract_from_browser, open_in_browser, write_cookie_file};
    let browsers = detect_browsers();
    let mut readable = Vec::new();
    let mut last_error: Option<String> = None;

    for b in &browsers {
        let cookie = match extract_from_browser(b.id).await {
            Ok(c) => c,
            Err(ExtractError::NotSignedIn { .. }) => {
                readable.push(b.id.to_string());
                continue;
            }
            Err(ExtractError::Other(msg)) => {
                last_error = Some(msg);
                continue;
            }
        };
        match YtMusic::from_cookie(&cookie).await {
            Ok(yt) if login_state(&yt).await == Some(false) => {
                readable.push(b.id.to_string());
            }
            Ok(yt) => {
                return match write_cookie_file(&cookie_file, &cookie) {
                    Ok(()) => AuthOutcome::SignedIn { client: yt },
                    Err(e) => AuthOutcome::Error {
                        message: format!("Signed in, but couldn't save the cookie file: {e}"),
                    },
                };
            }
            Err(e) => {
                last_error = Some(format!(
                    "Couldn't reach YouTube Music ({}). Check your internet connection and try again.",
                    explain(&e.to_string()).0
                ));
            }
        }
    }

    if readable.is_empty()
        && let Some(msg) = last_error
    {
        return AuthOutcome::Error { message: msg };
    }
    let specs = if readable.is_empty() {
        browsers.into_iter().map(|b| b.id.to_string()).collect()
    } else {
        readable
    };
    let browser_opened = open_in_browser("");
    AuthOutcome::NeedsBrowser { specs, browser_opened }
}

/// Extract cookies from a specific browser/profile, verify with YouTube Music
/// in memory, and only write `cookie_file` once YouTube confirms sign-in.
async fn verify_browser_login(spec: String, label: String, cookie_file: PathBuf) -> AuthOutcome {
    use crate::auth::{ExtractError, extract_from_browser, open_in_browser, write_cookie_file};
    let cookie = match extract_from_browser(&spec).await {
        Ok(c) => c,
        Err(ExtractError::NotSignedIn { .. }) => {
            let browser_opened = open_in_browser(&spec);
            return AuthOutcome::NeedsBrowser {
                specs: vec![spec],
                browser_opened,
            };
        }
        Err(ExtractError::Other(message)) => return AuthOutcome::Error { message },
    };

    match YtMusic::from_cookie(&cookie).await {
        Ok(yt) if login_state(&yt).await == Some(false) => {
            let browser_opened = open_in_browser(&spec);
            AuthOutcome::NeedsBrowser {
                specs: vec![spec],
                browser_opened,
            }
        }
        Ok(yt) => match write_cookie_file(&cookie_file, &cookie) {
            Ok(()) => AuthOutcome::SignedIn { client: yt },
            Err(e) => AuthOutcome::Error {
                message: format!("Signed in, but couldn't save the cookie file: {e}"),
            },
        },
        Err(e) => AuthOutcome::Error {
            message: format!(
                "Couldn't sign in with {label} ({}). Check your internet connection and try again.",
                explain(&e.to_string()).0
            ),
        },
    }
}

/// Quietly poll `specs` while waiting for the user to sign in on `music.youtube.com`.
async fn poll_sign_in(specs: Vec<String>, cookie_file: PathBuf) -> AuthOutcome {
    use crate::auth::{ExtractError, extract_from_browser, write_cookie_file};
    for spec in &specs {
        let cookie = match extract_from_browser(spec).await {
            Ok(c) => c,
            Err(ExtractError::NotSignedIn { .. } | ExtractError::Other(_)) => continue,
        };
        if let Ok(yt) = YtMusic::from_cookie(&cookie).await
            && login_state(&yt).await != Some(false)
        {
            return match write_cookie_file(&cookie_file, &cookie) {
                Ok(()) => AuthOutcome::SignedIn { client: yt },
                Err(e) => AuthOutcome::Error {
                    message: format!("Signed in, but couldn't save the cookie file: {e}"),
                },
            };
        }
    }
    AuthOutcome::PollPending
}

pub async fn run(cookie_file: PathBuf, mut reqs: UnboundedReceiver<YtmReq>, events: UnboundedSender<YtmEvent>) {
    let mut cookie_stamp = file_stamp(&cookie_file);
    let mut client = connect(&cookie_file, &events, None).await;
    // Requests run as separate tasks; one that finds the login rejected
    // reports it here so the client can drop back to signed-out.
    let (expired_tx, mut expired_rx) = tokio::sync::mpsc::unbounded_channel::<()>();
    // Interactive sign-in tasks (browser cookie extraction / paste) report here
    // so yt-dlp never blocks search or radio requests.
    let (auth_done_tx, mut auth_done_rx) = tokio::sync::mpsc::unbounded_channel::<AuthOutcome>();
    let mut watch = tokio::time::interval(std::time::Duration::from_secs(2));
    watch.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    loop {
        tokio::select! {
            req = reqs.recv() => {
                let Some(req) = req else { break };
                match req {
                    YtmReq::StartSignIn => {
                        let cookie_file = cookie_file.clone();
                        let auth_done_tx = auth_done_tx.clone();
                        tokio::spawn(async move {
                            let outcome = start_auto_sign_in(cookie_file).await;
                            let _ = auth_done_tx.send(outcome);
                        });
                        continue;
                    }
                    YtmReq::ImportBrowser { spec, label } => {
                        let cookie_file = cookie_file.clone();
                        let auth_done_tx = auth_done_tx.clone();
                        tokio::spawn(async move {
                            let outcome = verify_browser_login(spec, label, cookie_file).await;
                            let _ = auth_done_tx.send(outcome);
                        });
                        continue;
                    }
                    YtmReq::PollSignIn { specs } => {
                        let cookie_file = cookie_file.clone();
                        let auth_done_tx = auth_done_tx.clone();
                        tokio::spawn(async move {
                            let outcome = poll_sign_in(specs, cookie_file).await;
                            let _ = auth_done_tx.send(outcome);
                        });
                        continue;
                    }
                    YtmReq::OpenSignInLink { spec } => {
                        crate::auth::open_in_browser(spec.as_deref().unwrap_or(""));
                        continue;
                    }
                    YtmReq::SaveCookie { raw } => {
                        let cookie_file = cookie_file.clone();
                        let auth_done_tx = auth_done_tx.clone();
                        tokio::spawn(async move {
                            let outcome = match crate::auth::normalize_pasted_cookie(&raw) {
                                Err(e) => AuthOutcome::Error { message: e.to_string() },
                                Ok(cookie) => match YtMusic::from_cookie(&cookie).await {
                                    Ok(yt) if login_state(&yt).await == Some(false) => AuthOutcome::Error {
                                        message: "That cookie isn't signed in to YouTube Music (or has expired). \
                                                  Sign in at music.youtube.com and copy a fresh Cookie header."
                                            .into(),
                                    },
                                    Ok(yt) => match crate::auth::write_cookie_file(&cookie_file, &cookie) {
                                        Ok(()) => AuthOutcome::SignedIn { client: yt },
                                        Err(e) => AuthOutcome::Error {
                                            message: format!("Couldn't save cookie file: {e}"),
                                        },
                                    },
                                    Err(e) => AuthOutcome::Error {
                                        message: format!(
                                            "Couldn't sign in with the pasted cookie ({}).",
                                            explain(&e.to_string()).0
                                        ),
                                    },
                                },
                            };
                            let _ = auth_done_tx.send(outcome);
                        });
                        continue;
                    }
                    YtmReq::SignOut => {
                        match crate::auth::remove_cookie_file(&cookie_file) {
                            Ok(_) => {
                                cookie_stamp = file_stamp(&cookie_file);
                                tracing::info!("signed out by user");
                                let _ = events.send(YtmEvent::Auth {
                                    logged_in: false,
                                    message: None,
                                });
                                if matches!(client, Some(Client::Auth(_))) {
                                    client = YtMusic::new_unauthenticated().await.ok().map(Client::Anon);
                                }
                            }
                            Err(e) => {
                                let _ = events.send(YtmEvent::Failed {
                                    token: None,
                                    message: format!("Couldn't sign out: {e}"),
                                });
                            }
                        }
                        continue;
                    }
                    _ => {}
                }
                // If startup happened offline, reconnect lazily once the network is back.
                if client.is_none() {
                    client = connect(&cookie_file, &events, None).await;
                }
                let Some(c) = client.clone() else {
                    let _ = events.send(offline_event(req));
                    continue;
                };
                let events = events.clone();
                let expired_tx = expired_tx.clone();
                tokio::spawn(async move {
                    let (ev, auth_rejected) = handle(&c, req).await;
                    let _ = events.send(ev);
                    if auth_rejected && matches!(c, Client::Auth(_)) {
                        let _ = expired_tx.send(());
                    }
                });
            }
            Some(outcome) = auth_done_rx.recv() => {
                match outcome {
                    AuthOutcome::SignedIn { client: yt } => {
                        cookie_stamp = file_stamp(&cookie_file);
                        client = Some(Client::Auth(yt));
                        let _ = events.send(YtmEvent::Auth {
                            logged_in: true,
                            message: None,
                        });
                    }
                    AuthOutcome::NeedsBrowser { specs, browser_opened } => {
                        let _ = events.send(YtmEvent::AuthNeedsBrowser { specs, browser_opened });
                    }
                    AuthOutcome::PollPending => {
                        let _ = events.send(YtmEvent::AuthPollPending);
                    }
                    AuthOutcome::Error { message } => {
                        tracing::warn!("sign-in failed: {message}");
                        let _ = events.send(YtmEvent::Auth {
                            logged_in: matches!(client, Some(Client::Auth(_))),
                            message: Some(message),
                        });
                    }
                }
            }
            Some(()) = expired_rx.recv() => {
                if matches!(client, Some(Client::Auth(_))) {
                    tracing::warn!("login cookie rejected; continuing signed out");
                    let _ = events.send(YtmEvent::Auth {
                        logged_in: false,
                        message: Some(SIGNED_OUT.into()),
                    });
                    if let Ok(yt) = YtMusic::new_unauthenticated().await {
                        client = Some(Client::Anon(yt));
                    }
                }
            }
            _ = watch.tick() => {
                // A freshly imported cookie signs pova back in without a restart;
                // removing the cookie file drops back to signed-out.
                let stamp = file_stamp(&cookie_file);
                if stamp != cookie_stamp {
                    cookie_stamp = stamp;
                    if stamp.is_some() {
                        tracing::info!("cookie file changed; reconnecting");
                        if let Some(c) = connect(&cookie_file, &events, None).await {
                            client = Some(c);
                        }
                    } else if matches!(client, Some(Client::Auth(_))) {
                        tracing::info!("cookie file removed; continuing signed out");
                        let _ = events.send(YtmEvent::Auth {
                            logged_in: false,
                            message: None,
                        });
                        client = YtMusic::new_unauthenticated().await.ok().map(Client::Anon);
                    }
                }
            }
        }
    }
}

fn offline_event(req: YtmReq) -> YtmEvent {
    let message = "couldn't reach YouTube Music. Check your internet connection and try again.".to_string();
    match req {
        YtmReq::Load { token, .. } => YtmEvent::Failed {
            token: Some(token),
            message,
        },
        YtmReq::Suggest { query } => YtmEvent::Suggestions {
            query,
            items: Vec::new(),
        },
        YtmReq::Lyrics { video_id } => YtmEvent::Lyrics { video_id, text: None },
        _ => YtmEvent::Failed { token: None, message },
    }
}

/// Whether YouTube treats this client as signed in, from the `logged_in`
/// flag it puts in every response. None if the check itself failed.
async fn login_state(yt: &YtMusic<BrowserToken>) -> Option<bool> {
    use ytmapi_rs::query::GetLibraryPlaylistsQuery;
    let raw = yt.raw_json_query(GetLibraryPlaylistsQuery).await;
    match raw {
        Ok(raw) => {
            let json: serde_json::Value = serde_json::from_str(&raw).ok()?;
            let state = find_logged_in(&json);
            tracing::debug!("login check: logged_in = {state:?}");
            state
        }
        Err(e) => {
            tracing::debug!("login check failed: {e}");
            let (_, rejected) = explain(&e.to_string());
            if rejected { Some(false) } else { None }
        }
    }
}

/// Finds `{"key": "logged_in", "value": "0" | "1"}` anywhere in a response.
fn find_logged_in(v: &serde_json::Value) -> Option<bool> {
    match v {
        serde_json::Value::Object(map) => {
            if map.get("key").and_then(serde_json::Value::as_str) == Some("logged_in") {
                return match map.get("value").and_then(serde_json::Value::as_str) {
                    Some("1") => Some(true),
                    Some("0") => Some(false),
                    _ => None,
                };
            }
            map.values().find_map(find_logged_in)
        }
        serde_json::Value::Array(items) => items.iter().find_map(find_logged_in),
        _ => None,
    }
}

fn file_stamp(path: &std::path::Path) -> Option<std::time::SystemTime> {
    std::fs::metadata(path).and_then(|m| m.modified()).ok()
}

async fn connect(cookie_file: &PathBuf, events: &UnboundedSender<YtmEvent>, source: Option<&str>) -> Option<Client> {
    let mut auth_sent = false;
    if cookie_file.exists() {
        match YtMusic::from_cookie_file(cookie_file).await {
            // An expired cookie often isn't refused outright: YouTube just
            // answers as if signed out. Check before claiming "signed in".
            Ok(yt) if login_state(&yt).await == Some(false) => {
                tracing::warn!("cookie accepted but YouTube reports logged_in=0");
                let msg = match source {
                    Some(src) => format!(
                        "The cookies from {src} aren't signed in to YouTube Music (or expired). \
                         Open music.youtube.com in that browser, sign in, and try again."
                    ),
                    None => SIGNED_OUT.into(),
                };
                let _ = events.send(YtmEvent::Auth {
                    logged_in: false,
                    message: Some(msg),
                });
                auth_sent = true;
            }
            Ok(yt) => {
                let _ = events.send(YtmEvent::Auth {
                    logged_in: true,
                    message: None,
                });
                return Some(Client::Auth(yt));
            }
            Err(e) => {
                tracing::warn!("cookie auth failed: {e}");
                let msg = match source {
                    Some(src) => format!(
                        "Couldn't sign in with {src} ({}). Try another browser or check your internet connection.",
                        explain(&e.to_string()).0
                    ),
                    None => format!(
                        "Couldn't sign in with the saved cookie ({}). Continuing signed out; \
                         open the Library tab (b) to sign in again, or run `pova login`.",
                        explain(&e.to_string()).0
                    ),
                };
                let _ = events.send(YtmEvent::Auth {
                    logged_in: false,
                    message: Some(msg),
                });
                auth_sent = true;
            }
        }
    }
    match YtMusic::new_unauthenticated().await {
        Ok(yt) => {
            if !auth_sent {
                let _ = events.send(YtmEvent::Auth {
                    logged_in: false,
                    message: None,
                });
            }
            Some(Client::Anon(yt))
        }
        Err(e) => {
            tracing::error!("ytm connect failed: {e}");
            if !auth_sent {
                let _ = events.send(YtmEvent::Auth {
                    logged_in: false,
                    message: None,
                });
            }
            let _ = events.send(YtmEvent::Failed {
                token: None,
                message: format!("Couldn't reach YouTube Music: {}", explain(&e.to_string()).0),
            });
            None
        }
    }
}

/// Run one request. The flag is true when YouTube Music rejected the login.
async fn handle(client: &Client, req: YtmReq) -> (YtmEvent, bool) {
    let mut rejected = false;
    let mut fail = |context: &str, e: &dyn std::fmt::Display| {
        let (text, auth) = explain(&format!("{e:#}"));
        rejected |= auth;
        if context.is_empty() {
            text
        } else {
            format!("{context}: {text}")
        }
    };
    let ev = match req {
        YtmReq::Load { token, view } => match load(client, &view).await {
            Ok((title, items)) => YtmEvent::Loaded { token, title, items },
            Err(e) => {
                tracing::warn!("load {view:?}: {e:#}");
                YtmEvent::Failed {
                    token: Some(token),
                    message: fail("", &e),
                }
            }
        },
        YtmReq::Radio { seed } => {
            let res =
                with_yt!(client, yt => yt.get_watch_playlist_from_video_id(VideoID::from_raw(seed.clone())).await);
            match res {
                Ok(tracks) => YtmEvent::Radio {
                    tracks: tracks
                        .into_iter()
                        .filter(|t| t.video_id.get_raw() != seed)
                        .map(|t| Track {
                            video_id: t.video_id.get_raw().to_string(),
                            title: t.title,
                            artists: t.author,
                            album: None,
                            album_id: None,
                            duration: parse_duration(&t.duration),
                            thumbnail: best_thumb(&t.thumbnails),
                            liked: None,
                        })
                        .collect(),
                    seed,
                },
                Err(e) => YtmEvent::Failed {
                    token: None,
                    message: fail("Couldn't start radio", &e),
                },
            }
        }
        YtmReq::Rate { video_id, liked } => {
            let Client::Auth(yt) = client else {
                let ev = YtmEvent::Failed {
                    token: None,
                    message: "Liking songs needs you to be signed in. Open the Library tab (b) to sign in, \
                              or run `pova login`."
                        .into(),
                };
                return (ev, false);
            };
            let status = if liked {
                LikeStatus::Liked
            } else {
                LikeStatus::Indifferent
            };
            match yt.rate_song(VideoID::from_raw(video_id.clone()), status).await {
                Ok(()) => YtmEvent::Rated { video_id, liked },
                Err(e) => YtmEvent::Failed {
                    token: None,
                    message: fail(
                        if liked {
                            "Couldn't like the song"
                        } else {
                            "Couldn't remove the like"
                        },
                        &e,
                    ),
                },
            }
        }
        YtmReq::Dislike { video_id } => {
            let Client::Auth(yt) = client else {
                let ev = YtmEvent::Failed {
                    token: None,
                    message: "Disliking songs needs you to be signed in. Open the Library tab (b) to sign in, \
                              or run `pova login`."
                        .into(),
                };
                return (ev, false);
            };
            match yt
                .rate_song(VideoID::from_raw(video_id.clone()), LikeStatus::Disliked)
                .await
            {
                Ok(()) => YtmEvent::Disliked { video_id },
                Err(e) => YtmEvent::Failed {
                    token: None,
                    message: fail("Couldn't dislike the song", &e),
                },
            }
        }
        YtmReq::Lyrics { video_id } => {
            let text = with_yt!(client, yt => async {
                let id = yt.get_lyrics_id(VideoID::from_raw(video_id.clone())).await.ok()?;
                yt.get_lyrics(id).await.ok()
            }.await)
            .map(|l| l.lyrics)
            .filter(|l| !l.trim().is_empty());
            YtmEvent::Lyrics { video_id, text }
        }
        YtmReq::Suggest { query } => {
            let items = match with_yt!(client, yt => yt.get_search_suggestions(query.as_str()).await) {
                Ok(list) => list
                    .into_iter()
                    .map(|s| Suggestion {
                        text: s.get_text(),
                        history: s.suggestion_type == ytmapi_rs::common::SuggestionType::History,
                    })
                    .collect(),
                Err(e) => {
                    tracing::debug!("suggestions for {query:?}: {e}");
                    Vec::new()
                }
            };
            YtmEvent::Suggestions { query, items }
        }
        YtmReq::AddToPlaylist {
            playlist_id,
            playlist_title,
            video_id,
            title,
        } => match need_auth(client) {
            Err(e) => YtmEvent::Failed {
                token: None,
                message: e.to_string(),
            },
            Ok(yt) => {
                let res = yt
                    .add_video_items_to_playlist(
                        PlaylistID::from_raw(bare_playlist_id(&playlist_id)),
                        [VideoID::from_raw(video_id.clone())],
                    )
                    .await;
                match res {
                    Ok(_) => YtmEvent::PlaylistEdited {
                        message: format!("Added “{title}” to {playlist_title}"),
                        removed: None,
                    },
                    Err(e) => YtmEvent::Failed {
                        token: None,
                        message: fail(&format!("Couldn't add “{title}” to {playlist_title}"), &e),
                    },
                }
            }
        },
        YtmReq::CreatePlaylist { name, video_id, title } => match need_auth(client) {
            Err(e) => YtmEvent::Failed {
                token: None,
                message: e.to_string(),
            },
            Ok(yt) => {
                use ytmapi_rs::query::playlist::{CreatePlaylistQuery, PrivacyStatus};
                let query = CreatePlaylistQuery::new(&name, None, PrivacyStatus::Private)
                    .with_video_ids([VideoID::from_raw(video_id.clone())]);
                match yt.create_playlist(query).await {
                    Ok(_) => YtmEvent::PlaylistEdited {
                        message: format!("Created the private playlist {name} with “{title}”"),
                        removed: None,
                    },
                    Err(e) => YtmEvent::Failed {
                        token: None,
                        message: fail(&format!("Couldn't create the playlist {name}"), &e),
                    },
                }
            }
        },
        YtmReq::DeletePlaylist { playlist_id, title } => match need_auth(client) {
            Err(e) => YtmEvent::Failed {
                token: None,
                message: e.to_string(),
            },
            Ok(yt) => match yt
                .delete_playlist(PlaylistID::from_raw(bare_playlist_id(&playlist_id)))
                .await
            {
                Ok(()) => YtmEvent::PlaylistDeleted { playlist_id, title },
                Err(e) => YtmEvent::Failed {
                    token: None,
                    message: fail(
                        &format!("Couldn't delete {title} (you can only delete playlists you made)"),
                        &e,
                    ),
                },
            },
        },
        YtmReq::RemoveFromPlaylist {
            playlist_id,
            video_id,
            title,
        } => match need_auth(client) {
            Err(e) => YtmEvent::Failed {
                token: None,
                message: e.to_string(),
            },
            Ok(yt) => match remove_from_playlist(yt, &playlist_id, &video_id).await {
                Ok(true) => YtmEvent::PlaylistEdited {
                    message: format!("Removed “{title}” from the playlist (p adds it back)"),
                    removed: Some((playlist_id, video_id)),
                },
                Ok(false) => YtmEvent::Failed {
                    token: None,
                    message: format!(
                        "Couldn't find “{title}” in the first page of this playlist (pova only \
                         looks at the first ~100 songs for now). Remove it on music.youtube.com instead."
                    ),
                },
                Err(e) => YtmEvent::Failed {
                    token: None,
                    message: fail(&format!("Couldn't remove “{title}”"), &e),
                },
            },
        },
        YtmReq::StartSignIn
        | YtmReq::ImportBrowser { .. }
        | YtmReq::PollSignIn { .. }
        | YtmReq::OpenSignInLink { .. }
        | YtmReq::SaveCookie { .. }
        | YtmReq::SignOut => {
            unreachable!("handled directly in ytm::run")
        }
    };
    (ev, rejected)
}

/// Turn a ytmapi-rs or network error into a sentence a person can act on.
/// The flag is true when YouTube Music rejected the login (expired cookie).
pub fn explain(raw: &str) -> (String, bool) {
    let lower = raw.to_lowercase();
    if lower.contains("must be signed in")
        || lower.contains("error code 401")
        || lower.contains("not logged in")
        || lower.contains("login required")
    {
        return (SIGNED_OUT.to_string(), true);
    }
    if lower.contains("needs you to be signed in") {
        return (raw.to_string(), false);
    }
    if lower.contains("error sending request")
        || lower.contains("dns error")
        || lower.contains("failed to lookup")
        || lower.contains("connection refused")
        || lower.contains("connection reset")
        || lower.contains("timed out")
        || lower.contains("network is unreachable")
    {
        return (
            "couldn't reach YouTube Music. Check your internet connection and try again.".into(),
            false,
        );
    }
    // "Http error code 429 recieved in response. Message: <...>."
    if let Some(rest) = raw.split("Http error code ").nth(1) {
        let code: String = rest.chars().take_while(char::is_ascii_digit).collect();
        let message = rest
            .split_once('<')
            .and_then(|(_, m)| m.rsplit_once('>'))
            .map(|(m, _)| m.trim().trim_end_matches('.'))
            .filter(|m| !m.is_empty());
        let hint = match code.as_str() {
            "429" => " YouTube is rate-limiting requests; wait a minute and try again.",
            "403" => {
                " YouTube refused the request; signing in again from the Library tab (b) or `pova login` may help."
            }
            c if c.starts_with('5') => " YouTube Music is having trouble; try again in a moment.",
            _ => "",
        };
        return match message {
            Some(m) => (format!("YouTube Music answered with error {code}: {m}.{hint}"), false),
            None => (format!("YouTube Music answered with error {code}.{hint}"), false),
        };
    }
    (raw.to_string(), false)
}

/// Playlist ids without the "VL" browse prefix (what the edit endpoints want).
fn bare_playlist_id(id: &str) -> String {
    id.strip_prefix("VL").unwrap_or(id).to_string()
}

/// Removing needs the song's per-playlist "setVideoId", which ytmapi-rs
/// doesn't parse, so read it from the raw playlist page. Ok(false) when the
/// song isn't on the first page.
async fn remove_from_playlist(yt: &YtMusic<BrowserToken>, playlist_id: &str, video_id: &str) -> anyhow::Result<bool> {
    use ytmapi_rs::common::SetVideoID;
    let query = GetPlaylistTracksQuery::new(PlaylistID::from_raw(format!("VL{}", bare_playlist_id(playlist_id))));
    let raw = yt.raw_json_query(query).await?;
    let json: serde_json::Value = serde_json::from_str(&raw)?;
    let Some(set_id) = find_set_video_id(&json, video_id) else {
        return Ok(false);
    };
    yt.remove_playlist_items(
        PlaylistID::from_raw(bare_playlist_id(playlist_id)),
        [SetVideoID::from_raw(set_id)],
    )
    .await?;
    Ok(true)
}

/// `playlistItemData: {videoId, playlistSetVideoId}` for `video_id`.
fn find_set_video_id(v: &serde_json::Value, video_id: &str) -> Option<String> {
    match v {
        serde_json::Value::Object(map) => {
            if let Some(data) = map.get("playlistItemData")
                && data.get("videoId").and_then(serde_json::Value::as_str) == Some(video_id)
                && let Some(set) = data.get("playlistSetVideoId").and_then(serde_json::Value::as_str)
            {
                return Some(set.to_string());
            }
            map.values().find_map(|x| find_set_video_id(x, video_id))
        }
        serde_json::Value::Array(items) => items.iter().find_map(|x| find_set_video_id(x, video_id)),
        _ => None,
    }
}

fn need_auth(client: &Client) -> anyhow::Result<&YtMusic<BrowserToken>> {
    match client {
        Client::Auth(yt) => Ok(yt),
        Client::Anon(_) => anyhow::bail!(
            "Your library needs you to be signed in. Open the Library tab (b) to sign in, or run `pova login`."
        ),
    }
}

async fn load(client: &Client, view: &View) -> anyhow::Result<(String, Vec<Item>)> {
    Ok(match view {
        View::Search { filter, query } => {
            let (items, corrected) = search_corrected(client, *filter, query).await?;
            match corrected {
                Some(c) => (format!("{} · showing results for “{c}”", filter.label()), items),
                None => (format!("{} · {}", filter.label(), query), items),
            }
        }
        View::Album(id) => {
            let album = with_yt!(client, yt => yt.get_album(AlbumID::from_raw(id.clone())).await)?;
            let artists = album
                .artists
                .iter()
                .map(|a| a.name.as_str())
                .collect::<Vec<_>>()
                .join(", ");
            let thumb = best_thumb(&album.thumbnails);
            let items = album
                .tracks
                .into_iter()
                .map(|s| {
                    Item::Track(Track {
                        video_id: s.video_id.get_raw().to_string(),
                        title: s.title,
                        artists: artists.clone(),
                        album: Some(album.title.clone()),
                        album_id: Some(id.clone()),
                        duration: parse_duration(&s.duration),
                        thumbnail: thumb.clone(),
                        liked: like_flag(&s.like_status),
                    })
                })
                .collect();
            (format!("{} — {} ({})", album.title, artists, album.year), items)
        }
        View::Artist(id) => {
            let artist = with_yt!(client, yt => yt.get_artist(ArtistChannelID::from_raw(id.clone())).await)?;
            let mut items = Vec::new();
            let top = artist.top_releases;
            if let Some(songs) = top.songs {
                items.push(Item::Header("Top songs".into()));
                for s in songs.results {
                    items.push(Item::Track(Track {
                        video_id: s.video_id.get_raw().to_string(),
                        title: s.title,
                        artists: join_artists(&s.artists),
                        album: Some(s.album.name.clone()),
                        album_id: Some(s.album.id.get_raw().to_string()),
                        duration: None,
                        thumbnail: None,
                        liked: like_flag(&s.like_status),
                    }));
                }
            }
            for (label, albums) in [("Albums", top.albums), ("Singles & EPs", top.singles)] {
                let Some(albums) = albums else { continue };
                items.push(Item::Header(label.into()));
                for a in albums.results {
                    items.push(Item::Album(AlbumRef {
                        id: a.album_id.get_raw().to_string(),
                        title: a.title,
                        subtitle: a.year,
                    }));
                }
            }
            (artist.name, items)
        }
        View::Playlist(id) => {
            let tracks = playlist_tracks(client, id).await?;
            let title = if id.trim_start_matches("VL") == "LM" {
                "Liked songs"
            } else {
                "Playlist"
            }
            .to_string();
            (title, tracks)
        }
        View::LikedSongs => {
            need_auth(client)?;
            ("Liked songs".into(), playlist_tracks(client, "LM").await?)
        }
        View::LibraryPlaylists => {
            let yt = need_auth(client)?;
            let lists = yt.get_library_playlists().await?;
            let items = lists
                .into_iter()
                .map(|p| {
                    Item::Playlist(PlaylistRef {
                        id: p.playlist_id.get_raw().to_string(),
                        title: p.title,
                        subtitle: format!("{} · {}", p.author, p.tracks),
                    })
                })
                .collect();
            ("Your playlists".into(), items)
        }
        View::History => {
            let yt = need_auth(client)?;
            let periods = yt.get_history().await?;
            let mut items = Vec::new();
            for period in periods {
                items.push(Item::Header(period.period_name));
                for it in period.items {
                    match it {
                        HistoryItem::Song(s) => items.push(Item::Track(Track {
                            video_id: s.video_id.get_raw().to_string(),
                            title: s.title,
                            artists: join_artists(&s.artists),
                            album: Some(s.album.name.clone()),
                            album_id: Some(s.album.id.get_raw().to_string()),
                            duration: parse_duration(&s.duration),
                            thumbnail: best_thumb(&s.thumbnails),
                            liked: like_flag(&s.like_status),
                        })),
                        HistoryItem::Video(v) => items.push(Item::Track(Track {
                            video_id: v.video_id.get_raw().to_string(),
                            title: v.title,
                            artists: v.channel_name,
                            album: None,
                            album_id: None,
                            duration: parse_duration(&v.duration),
                            thumbnail: best_thumb(&v.thumbnails),
                            liked: like_flag(&v.like_status),
                        })),
                        _ => {}
                    }
                }
            }
            ("History".into(), items)
        }
    })
}

async fn playlist_tracks(client: &Client, id: &str) -> anyhow::Result<Vec<Item>> {
    // The browse endpoint wants the "VL"-prefixed form of the playlist id.
    let browse_id = if id.starts_with("VL") {
        id.to_string()
    } else {
        format!("VL{id}")
    };
    let query = GetPlaylistTracksQuery::new(PlaylistID::from_raw(browse_id));
    // Keep every page that parsed: one odd item on a later page (ytmapi-rs
    // parse error) must not throw away the whole list.
    let results: Vec<ytmapi_rs::Result<Vec<PlaylistItem>>> =
        with_yt!(client, yt => yt.stream(&query).take(MAX_PAGES).collect().await);
    let mut pages: Vec<Vec<PlaylistItem>> = Vec::new();
    for r in results {
        match r {
            Ok(page) => pages.push(page),
            Err(e) if pages.is_empty() => return Err(e.into()),
            Err(e) => {
                tracing::warn!("playlist {id}: stopping after {} pages: {e}", pages.len());
                break;
            }
        }
    }
    Ok(pages
        .into_iter()
        .flatten()
        .filter_map(playlist_item)
        .map(Item::Track)
        .collect())
}

fn playlist_item(item: PlaylistItem) -> Option<Track> {
    match item {
        PlaylistItem::Song(s) if s.is_available => Some(Track {
            video_id: s.video_id.get_raw().to_string(),
            title: s.title,
            artists: join_artists(&s.artists),
            album: Some(s.album.name.clone()),
            album_id: Some(s.album.id.get_raw().to_string()),
            duration: parse_duration(&s.duration),
            thumbnail: best_thumb(&s.thumbnails),
            liked: like_flag(&s.like_status),
        }),
        PlaylistItem::Video(v) if v.is_available => Some(Track {
            video_id: v.video_id.get_raw().to_string(),
            title: v.title,
            artists: v.channel_name,
            album: None,
            album_id: None,
            duration: parse_duration(&v.duration),
            thumbnail: best_thumb(&v.thumbnails),
            liked: like_flag(&v.like_status),
        }),
        _ => None,
    }
}

/// Search like the YouTube Music search bar: if the query is misspelled, YTM
/// answers with a "Did you mean / Showing results for" block that ytmapi-rs
/// parses as zero results. In that case take YTM's corrected query from the
/// raw response and search again with it.
async fn search_corrected(
    client: &Client,
    filter: SearchFilter,
    query: &str,
) -> anyhow::Result<(Vec<Item>, Option<String>)> {
    let items = search(client, filter, query).await?;
    if !items.is_empty() {
        return Ok((items, None));
    }
    let Some(corrected) = corrected_query(client, query).await else {
        return Ok((items, None));
    };
    if corrected.eq_ignore_ascii_case(query.trim()) {
        return Ok((items, None));
    }
    tracing::info!("search {query:?}: using YTM correction {corrected:?}");
    let items = search(client, filter, &corrected).await?;
    Ok((items, Some(corrected)))
}

/// YTM's spelling correction for `query`, if it offers one.
async fn corrected_query(client: &Client, query: &str) -> Option<String> {
    use ytmapi_rs::query::SearchQuery;
    use ytmapi_rs::query::search::BasicSearch;
    let q: SearchQuery<BasicSearch> = query.to_string().into();
    let raw = with_yt!(client, yt => yt.raw_json_query(q).await).ok()?;
    let json: serde_json::Value = serde_json::from_str(&raw).ok()?;
    find_correction(&json)
}

fn find_correction(v: &serde_json::Value) -> Option<String> {
    match v {
        serde_json::Value::Object(map) => map
            .get("correctedQueryEndpoint")
            .and_then(|e| e.pointer("/searchEndpoint/query"))
            .and_then(serde_json::Value::as_str)
            .map(str::to_string)
            .or_else(|| map.values().find_map(find_correction)),
        serde_json::Value::Array(items) => items.iter().find_map(find_correction),
        _ => None,
    }
}

async fn search(client: &Client, filter: SearchFilter, query: &str) -> anyhow::Result<Vec<Item>> {
    let q = query.to_string();
    Ok(match filter {
        SearchFilter::Songs => with_yt!(client, yt => yt.search_songs(q).await)?
            .into_iter()
            .map(|s| {
                Item::Track(Track {
                    video_id: s.video_id.get_raw().to_string(),
                    title: s.title,
                    artists: s.artist,
                    album: s.album.as_ref().map(|a| a.name.clone()),
                    album_id: s.album.as_ref().map(|a| a.id.get_raw().to_string()),
                    duration: parse_duration(&s.duration),
                    thumbnail: best_thumb(&s.thumbnails),
                    liked: None,
                })
            })
            .collect(),
        SearchFilter::Videos => with_yt!(client, yt => yt.search_videos(q).await)?
            .into_iter()
            .filter_map(|v| match v {
                SearchResultVideo::Video {
                    title,
                    channel_name,
                    video_id,
                    length,
                    thumbnails,
                    ..
                } => Some(Item::Track(Track {
                    video_id: video_id.get_raw().to_string(),
                    title,
                    artists: channel_name,
                    album: None,
                    album_id: None,
                    duration: parse_duration(&length),
                    thumbnail: best_thumb(&thumbnails),
                    liked: None,
                })),
                _ => None,
            })
            .collect(),
        SearchFilter::Albums => with_yt!(client, yt => yt.search_albums(q).await)?
            .into_iter()
            .map(|a| {
                Item::Album(AlbumRef {
                    id: a.album_id.get_raw().to_string(),
                    title: a.title,
                    subtitle: format!("{} · {}", a.artist, a.year),
                })
            })
            .collect(),
        SearchFilter::Artists => with_yt!(client, yt => yt.search_artists(q).await)?
            .into_iter()
            .map(|a| {
                Item::Artist(ArtistRef {
                    id: a.browse_id.get_raw().to_string(),
                    name: a.artist,
                    subtitle: a.subscribers.unwrap_or_default(),
                })
            })
            .collect(),
        SearchFilter::Playlists => with_yt!(client, yt => yt.search_playlists(q).await)?
            .into_iter()
            .filter_map(|p| match p {
                SearchResultPlaylist::Featured(p) => Some(Item::Playlist(PlaylistRef {
                    id: p.playlist_id.get_raw().to_string(),
                    title: p.title,
                    subtitle: format!("{} · {}", p.author, p.songs),
                })),
                SearchResultPlaylist::Community(p) => Some(Item::Playlist(PlaylistRef {
                    id: p.playlist_id.get_raw().to_string(),
                    title: p.title,
                    subtitle: format!("{} · {}", p.author, p.views),
                })),
                _ => None,
            })
            .collect(),
    })
}

fn best_thumb(thumbs: &[Thumbnail]) -> Option<String> {
    thumbs.iter().max_by_key(|t| t.width * t.height).map(|t| t.url.clone())
}

fn join_artists(a: &[ytmapi_rs::parse::ParsedSongArtist]) -> String {
    a.iter().map(|a| a.name.as_str()).collect::<Vec<_>>().join(", ")
}

fn like_flag(s: &LikeStatus) -> Option<bool> {
    match s {
        LikeStatus::Liked => Some(true),
        LikeStatus::Indifferent => Some(false),
        LikeStatus::Disliked => Some(false),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Hits the real API: `cargo test -- --ignored live_api`.
    #[tokio::test]
    #[ignore]
    async fn live_api() {
        let client = Client::Anon(YtMusic::new_unauthenticated().await.unwrap());
        let songs = search(&client, SearchFilter::Songs, "daft punk get lucky")
            .await
            .unwrap();
        let first = songs.iter().find_map(Item::as_track).expect("a song").clone();
        println!("song: {first:?}");
        for f in [
            SearchFilter::Videos,
            SearchFilter::Albums,
            SearchFilter::Artists,
            SearchFilter::Playlists,
        ] {
            let r = search(&client, f, "daft punk").await.unwrap();
            println!("{f:?}: {} results, first {:?}", r.len(), r.first());
            assert!(!r.is_empty(), "{f:?} empty");
        }
        let album_id = first.album_id.clone().expect("album id");
        let (title, tracks) = load(&client, &View::Album(album_id)).await.unwrap();
        println!("album {title}: {} tracks", tracks.len());
        assert!(!tracks.is_empty());

        let artists = search(&client, SearchFilter::Artists, "daft punk").await.unwrap();
        if let Some(Item::Artist(a)) = artists.first() {
            let (name, items) = load(&client, &View::Artist(a.id.clone())).await.unwrap();
            println!("artist {name}: {} items", items.len());
        }
        let pls = search(&client, SearchFilter::Playlists, "daft punk").await.unwrap();
        if let Some(Item::Playlist(p)) = pls.first() {
            let (_, items) = load(&client, &View::Playlist(p.id.clone())).await.unwrap();
            println!("playlist {} ({}): {} tracks", p.title, p.id, items.len());
            assert!(!items.is_empty());
        }
        match handle(
            &client,
            YtmReq::Radio {
                seed: first.video_id.clone(),
            },
        )
        .await
        .0
        {
            YtmEvent::Radio { tracks, .. } => {
                println!("radio: {} tracks", tracks.len());
                assert!(!tracks.is_empty());
            }
            other => panic!("radio: {other:?}"),
        }
        match handle(
            &client,
            YtmReq::Lyrics {
                video_id: first.video_id.clone(),
            },
        )
        .await
        .0
        {
            YtmEvent::Lyrics { text, .. } => {
                println!("ytm lyrics: {:?}", text.map(|t| t.chars().take(60).collect::<String>()))
            }
            other => panic!("lyrics: {other:?}"),
        }
    }
}

#[cfg(test)]
mod correction_tests {
    use super::{explain, find_correction};

    #[test]
    fn finds_did_you_mean_query() {
        let json: serde_json::Value = serde_json::from_str(
            r#"{"contents":{"tabs":[{"sections":[{"itemSectionRenderer":{"contents":[{"didYouMeanRenderer":
            {"correctedQueryEndpoint":{"searchEndpoint":{"query":"call me when you're sober","params":"x"}}}}]}}]}]}}"#,
        )
        .unwrap();
        assert_eq!(find_correction(&json).as_deref(), Some("call me when you're sober"));
        assert_eq!(find_correction(&serde_json::json!({"a": [1, {"b": null}]})), None);
    }

    #[test]
    fn finds_set_video_id() {
        use super::{bare_playlist_id, find_set_video_id};
        let json = serde_json::json!({"contents": [{"musicResponsiveListItemRenderer": {
            "playlistItemData": {"videoId": "aaa", "playlistSetVideoId": "SET_A"}}},
            {"musicResponsiveListItemRenderer": {
            "playlistItemData": {"videoId": "bbb", "playlistSetVideoId": "SET_B"}}}]});
        assert_eq!(find_set_video_id(&json, "bbb").as_deref(), Some("SET_B"));
        assert_eq!(find_set_video_id(&json, "zzz"), None);
        assert_eq!(bare_playlist_id("VLPL123"), "PL123");
        assert_eq!(bare_playlist_id("PL123"), "PL123");
    }

    #[test]
    fn reads_logged_in_flag() {
        use super::find_logged_in;
        let out = serde_json::json!({"responseContext": {"serviceTrackingParams": [
            {"service": "GFEEDBACK", "params": [{"key": "logged_in", "value": "0"}]}]}});
        assert_eq!(find_logged_in(&out), Some(false));
        let inn = serde_json::json!({"a": [{"key": "logged_in", "value": "1"}]});
        assert_eq!(find_logged_in(&inn), Some(true));
        assert_eq!(find_logged_in(&serde_json::json!({"key": "other"})), None);
    }

    #[test]
    fn explains_expired_login() {
        let raw =
            "Http error code 401 recieved in response. Message: <You must be signed in to perform this operation.>.";
        let (text, auth) = explain(raw);
        assert!(auth);
        assert!(text.contains("pova login"));
    }

    #[test]
    fn explains_http_errors_readably() {
        let (text, auth) = explain("Http error code 429 recieved in response. Message: <Too many requests>.");
        assert!(!auth);
        assert!(text.starts_with("YouTube Music answered with error 429: Too many requests."));
        assert!(text.contains("wait a minute"));
        let (text, _) = explain("error sending request for url (https://music.youtube.com/...)");
        assert!(text.contains("internet connection"));
        assert_eq!(explain("something odd").0, "something odd");
    }

    #[test]
    fn offline_requests_fail_cleanly_instead_of_hanging() {
        use super::{SearchFilter, View, YtmEvent, YtmReq, offline_event};
        let ev = offline_event(YtmReq::Load {
            token: 7,
            view: View::Search {
                filter: SearchFilter::Songs,
                query: "x".into(),
            },
        });
        assert!(matches!(ev, YtmEvent::Failed { token: Some(7), .. }));
        let ev = offline_event(YtmReq::Suggest { query: "ab".into() });
        assert!(matches!(ev, YtmEvent::Suggestions { items, .. } if items.is_empty()));
        let ev = offline_event(YtmReq::Lyrics { video_id: "v1".into() });
        assert!(matches!(ev, YtmEvent::Lyrics { text: None, .. }));
    }
}

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
    Lyrics {
        video_id: String,
    },
    /// Autocomplete suggestions for a partially typed query.
    Suggest {
        query: String,
    },
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
    Lyrics {
        video_id: String,
        text: Option<String>,
    },
    Failed {
        token: Option<u64>,
        message: String,
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

pub async fn run(cookie_file: PathBuf, mut reqs: UnboundedReceiver<YtmReq>, events: UnboundedSender<YtmEvent>) {
    let client = connect(&cookie_file, &events).await;
    let Some(client) = client else { return };
    while let Some(req) = reqs.recv().await {
        let client = client.clone();
        let events = events.clone();
        tokio::spawn(async move {
            let ev = handle(&client, req).await;
            let _ = events.send(ev);
        });
    }
}

async fn connect(cookie_file: &PathBuf, events: &UnboundedSender<YtmEvent>) -> Option<Client> {
    if cookie_file.exists() {
        match YtMusic::from_cookie_file(cookie_file).await {
            Ok(yt) => {
                let _ = events.send(YtmEvent::Auth {
                    logged_in: true,
                    message: None,
                });
                return Some(Client::Auth(yt));
            }
            Err(e) => {
                tracing::warn!("cookie auth failed: {e}");
                let _ = events.send(YtmEvent::Auth {
                    logged_in: false,
                    message: Some(format!("Cookie login failed ({e}); continuing signed out")),
                });
            }
        }
    }
    match YtMusic::new_unauthenticated().await {
        Ok(yt) => {
            if !cookie_file.exists() {
                let _ = events.send(YtmEvent::Auth {
                    logged_in: false,
                    message: None,
                });
            }
            Some(Client::Anon(yt))
        }
        Err(e) => {
            tracing::error!("ytm connect failed: {e}");
            let _ = events.send(YtmEvent::Failed {
                token: None,
                message: format!("Could not reach YouTube Music: {e}"),
            });
            None
        }
    }
}

async fn handle(client: &Client, req: YtmReq) -> YtmEvent {
    match req {
        YtmReq::Load { token, view } => match load(client, &view).await {
            Ok((title, items)) => YtmEvent::Loaded { token, title, items },
            Err(e) => {
                tracing::warn!("load {view:?}: {e:#}");
                YtmEvent::Failed {
                    token: Some(token),
                    message: format!("{e:#}"),
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
                    message: format!("Radio failed: {e}"),
                },
            }
        }
        YtmReq::Rate { video_id, liked } => {
            let Client::Auth(yt) = client else {
                return YtmEvent::Failed {
                    token: None,
                    message: "Liking songs needs a cookie login (see README)".into(),
                };
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
                    message: format!("Like failed: {e}"),
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
    }
}

fn need_auth(client: &Client) -> anyhow::Result<&YtMusic<BrowserToken>> {
    match client {
        Client::Auth(yt) => Ok(yt),
        Client::Anon(_) => anyhow::bail!("Library needs a cookie login — see README (~/.config/pova/cookie.txt)"),
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
    use super::find_correction;

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
}

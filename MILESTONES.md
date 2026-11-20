# Milestones

These are the build milestones from the plan, with the code that delivers each
one and the evidence that verifies it.

Note: development happened in one session, and clippy was first run after all
seven milestones were written. Since then, `scripts/check.sh` (rustfmt check,
`clippy -D warnings`, tests) gates every commit.

| # | Milestone | Code | Verification |
|---|---|---|---|
| 1 | btop-style skeleton: 3-box layout, btop themes, keymap, tabs | `ui/mod.rs`, `theme.rs`, `keymap.rs`, `app.rs` | `theme::all_btop_themes_parse` (every btop theme), `ui::wide_layout_matches_sketch`, `ui::renders_everywhere` (5 sizes × 4 viz modes × 5 tabs × 5 box layouts plus overlays), keymap tests |
| 2 | mpv backend: play, pause, seek, volume, next; progress bar | `player/mpv.rs`, `player/queue.rs` | `mpv::parses_property_events`, queue tests; manual tmux run: playback, pause, ±30 s seek, gapless advance to the preloaded track, volume |
| 3 | Visualizer: capture mpv's PipeWire node, cavacore port, bars | `viz/{capture,core,mod,render}.rs` | `core::tones_land_in_expected_bars`, `core::silence_decays_to_zero`, render tests; manual: "● live" with music, stream-only capture confirmed against the default-sink monitor |
| 4 | YTM: search, play, append; queue; autoplay radio | `ytm.rs`, `app.rs` | `ytm::live_api` (`--ignored`, real API): all 5 filters, album, artist, playlist, radio, lyrics; manual: search → play → radio filled the queue |
| 5 | Album art (kitty) and synced lyrics | `art.rs`, `ui/tabs.rs`, `lyrics.rs` | `ui::kitty_album_art_renders_placeholders` (kitty transmit escape plus unicode placeholders through the threaded resize path), `art::letterboxed_thumbnail…`, `lyrics::lrc_parsing`; manual: LRCLIB synced lyrics highlighted |
| 6 | Cookie auth and Library tab | `ytm.rs` (`connect`, `need_auth`), `ui/tabs.rs::library` | Signed-out path rendered and tested; signed-in path needs the user's cookie (not run) |
| 7 | Polish: help, theme picker, config, mouse, signals, teardown | `ui/overlay.rs`, `config.rs`, `main.rs` | Manual: theme picker plus saved state; clean teardown after `Q`, SIGHUP/SIGTERM and `kill -9` (no mpv or pw-record left behind); idle CPU about 2 % |

Run the gate with `./scripts/check.sh`.

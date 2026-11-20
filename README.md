# pova

A btop-style YouTube Music player for the terminal, with a built-in
[cava](https://github.com/karlstav/cava)-style audio visualizer. Written in Rust.

```
╭─┐¹visualizer┌──────────────────────┐v bars┌─╮╭─²┐now playing┌┐queue┌┐lyrics┌┐search┌┐library┌─╮
│                    ▄▄                      ││         ┌──────────────────┐                  │
│                 ▁▁ ██ ██                   ││         │    album art     │                  │
│              ▆▆ ██ ██ ██ ▃▃                ││         └──────────────────┘                  │
│           ▅▅ ██ ██ ██ ██ ██ ▃▃             ││     Get Lucky (feat. Pharrell Williams)       │
│ ▇▇ ▅▅  ▁▁ ██ ██ ██ ██ ██ ██ ██ ▁▁ ▂▂ ██     ││   ━━━━━━━━━━━━●────────────────────────────   │
│ ██ ██ ▇▇ ██ ██ ██ ██ ██ ██ ██ ██ ██ ██ ██   │╰───────────────────────────────┐○ signed out┌─╯
│ ██ ██ ██ ██ ██ ██ ██ ██ ██ ██ ██ ██ ██ ██   │╭─┐³controls┌─────────────────────────────────╮
│ ██ ██ ██ ██ ██ ██ ██ ██ ██ ██ ██ ██ ██ ██   ││        ⇄     ⏮     ⏸     ⏭     ↻          │
╰─┐● live┌────────────────────────────────────╯╰──────────────────────────────────────────╯
```

## Features

- **Visualizer**: a pure-Rust port of cava's `cavacore` (same FFT, bar
  distribution, gravity and autosens). It captures **only mpv's own PipeWire
  stream**, so Discord and browser audio don't show up. Modes: stereo bars,
  mirror, mono, and a Braille waveform.
- **YouTube Music**: search songs, videos, albums, artists and playlists;
  browse albums, artists and playlists; play now, play next, or add to the
  queue; radio; autoplay when the queue runs out.
- **Gapless playback** through a headless mpv: pova owns the queue and preloads
  the next track.
- **Queue**: reorder, remove, clear, shuffle (reversible), repeat off/all/one.
- **Lyrics**: time-synced from [LRCLIB](https://lrclib.net), highlighted and
  auto-scrolled. Falls back to YouTube Music's plain lyrics.
- **Album art**: real pixels in kitty (graphics protocol), halfblocks elsewhere.
- **Library** (with cookie login): your playlists, liked songs, history, and
  like/unlike.
- **btop look and themes**: rounded boxes, superscript box hotkeys, and tabs in
  the border. pova reads **btop `.theme` files directly**, so every btop theme
  works, with live preview.
- Mouse support: click tabs, buttons, list rows and the progress bar; scroll
  lists.

## Requirements (Fedora)

```sh
sudo dnf install mpv yt-dlp pipewire-utils nodejs   # nodejs (or deno) lets yt-dlp solve YouTube's JS challenges
sudo dnf install mpv-mpris                          # optional: KDE media keys / Plasma media widget
sudo dnf install btop                               # optional: ships ~40 themes in /usr/share/btop/themes
```

`pw-record` and `pw-dump` (from `pipewire-utils`) are used for audio capture.
No PipeWire development headers are needed.

## Install

```sh
cargo install --path .
pova                       # or: pova --play "daft punk around the world"
```

## Signing in (library, likes, history)

Search, playback, radio and lyrics all work without an account. For your
library:

1. Open <https://music.youtube.com> in your browser while signed in.
2. Open DevTools → **Network**, click any request to `music.youtube.com`, and
   copy the full value of the **`Cookie`** request header.
3. Paste it into `~/.config/pova/cookie.txt` (one line), then `chmod 600` it.
4. Restart pova. The player box shows **● signed in**.

The cookie stays on your machine and is only sent to YouTube Music.

## Keys

Press `?` in pova to see all bindings, including your overrides.

| Key | Action | Key | Action |
|---|---|---|---|
| `space` | play / pause | `n` `q` `l` `/`/`s` `b` | tabs: now playing, queue, lyrics, search, library |
| `←` `→` / `,` `.` | seek ∓5 s | `tab` / `shift+tab` | next / previous tab |
| `H` `L` / `shift+←→` | seek ∓30 s | `1` `2` `3` | show/hide visualizer, player, controls |
| `<` `>` / `P` `N` | previous / next track | `j` `k` `↑` `↓` `g` `G` | move in lists |
| `+` `-` / `m` | volume / mute | `enter` | play / open |
| `S` / `R` | shuffle / repeat | `a` / `A` | add to queue / play next |
| `f` | like current track | `r` | start radio from selection |
| `v` | visualizer mode | `d` / `J` `K` / `C` | queue: remove / move / clear |
| `t` | theme picker | `[` `]` | search filter / library section |
| `esc` | back / menu | `c` | jump to current track (lyrics: resume auto-scroll) |
| `Q`, `ctrl+c` | quit | `F5` | reload view |

While typing in the search box, `tab` changes the filter, `enter` searches, and
`esc` or `↓` returns to the results.

## Configuration

`~/.config/pova/config.toml` is created on first run:

```toml
theme = "Default"          # any btop theme name, e.g. "dracula", "gruvbox_dark"
theme_background = true    # false = keep the terminal's (transparent) background
rounded_corners = true
volume = 70
autoplay = true            # radio continues when the queue ends
album_art = true
image_protocol = "auto"    # auto | kitty | sixel | iterm2 | halfblocks
mpv_args = []              # e.g. ["--ytdl-raw-options-append=cookies-from-browser=firefox"]

[viz]
mode = "bars"              # bars | mirror | mono | wave
source = "mpv"             # mpv = only pova's audio, system = everything (like cava)
bar_width = 2
bar_spacing = 1
noise_reduction = 0.77     # cava's smoothing, 0..1
framerate = 60
low_cut_off = 50
high_cut_off = 10000
sample_rate = 44100

[keys]                     # override any action (see `?` for names)
next = ["N", "ctrl+right"]
```

The last theme, volume, visualizer mode and box visibility are remembered in
`~/.local/state/pova/state.toml`.

### Themes

Themes are looked up in `~/.config/pova/themes/`, `~/.config/btop/themes/` and
`/usr/share/btop/themes/`. Run `pova --list-themes` to see them. btop keys map
to pova like this:

| btop key | pova |
|---|---|
| `main_bg`, `main_fg`, `title`, `hi_fg`, `selected_*`, `inactive_fg`, `div_line` | same meaning |
| `cpu_box`, `proc_box`, `mem_box` | visualizer, player and controls borders |
| `cpu_start/mid/end` | visualizer gradient (override with `viz_start/mid/end`) |
| `download_start/mid/end` | progress and volume gradient (override with `progress_*`) |

## How it works

```
keys/mouse ─► App (reducer, 60 fps render) ◄─ events ─┬─ player actor ── mpv (JSON IPC)
                                                      ├─ ytm actor ───── ytmapi-rs
                                                      ├─ lyrics actor ── LRCLIB
                                                      └─ art actor ───── reqwest + image
pw-record (mpv's node) ─► capture thread ─(lock-free rtrb ring)─► DSP thread (cavacore port) ─► watch ─► UI
```

Logs go to `~/.cache/pova/pova.log` and never to the TUI. Use
`pova --log-level debug` for more detail.

## Development

```sh
cargo test                          # unit tests; parses every btop theme
cargo test -- --ignored live_api    # hits the real YouTube Music API
cargo clippy --all-targets
```

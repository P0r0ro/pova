# Configuration and themes

`~/.config/pova/config.toml` is created on first run with every option in it:

```toml
theme = "Default"          # any btop theme name: "dracula", "gruvbox_dark", "tokyo-night"...
theme_background = true    # false keeps your terminal's own (maybe transparent) background
rounded_corners = true
volume = 70
autoplay = true            # keep playing similar songs when the queue ends
album_art = true
image_protocol = "auto"    # auto | kitty | sixel | iterm2 | halfblocks
mpv_args = []              # extra flags for mpv, if you want them
resume = true              # start where you left off: same queue, same spot, paused
normalize_loudness = true  # play every song at the same loudness
notifications = false      # desktop popup with the cover art when the song changes

[viz]
mode = "bars"              # bars | mirror | mono | wave
source = "mpv"             # "mpv" = only pova's audio; "system" = everything, like plain cava
bar_width = 2
bar_spacing = 1
noise_reduction = 0.77     # cava's smoothing: higher is calmer
framerate = 60
low_cut_off = 50
high_cut_off = 10000
sample_rate = 44100

[keys]                     # remap anything; press ? in pova for action names
next = ["N", "ctrl+right"]
```

pova remembers your last theme, volume, visualizer mode, box layout and menu
settings in `~/.local/state/pova/state.toml`.

**Even loudness and notifications** can also be switched in pova's `esc`
menu. What you pick there wins over `config.toml`.

- `normalize_loudness` runs mpv's loudness filter (EBU R128, aiming at the
  -14 LUFS YouTube itself uses), so a quiet acoustic song and a loud master
  play at about the same volume. If you set your own audio filter with
  `--af=` in `mpv_args`, that replaces pova's.
- `notifications` needs `notify-send` (the `libnotify` package; `libnotify-bin`
  on Debian and Ubuntu). Each new song replaces the previous popup, so they
  don't pile up.

**Themes** are looked up in `~/.config/pova/themes/`, `~/.config/btop/themes/`
and `/usr/share/btop/themes/`. Run `pova --list-themes` to see what you have.
To make your own, copy a btop theme and change it. These are the keys pova
reads:

| btop key | used by pova for |
|---|---|
| `main_bg`, `main_fg`, `title`, `hi_fg`, `selected_bg/fg`, `inactive_fg`, `div_line` | the same things as in btop |
| `cpu_box`, `proc_box`, `mem_box` | the visualizer, player and controls borders |
| `cpu_start`, `cpu_mid`, `cpu_end` | the visualizer's colour gradient |
| `download_start/mid/end` | the progress and volume bars |

pova also understands a few keys btop doesn't have: `viz_start/mid/end` and
`progress_start/mid/end`.

The full list of action names for `[keys]` is in [keys.md](keys.md).

Back to the [README](../README.md).

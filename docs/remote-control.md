# Controlling pova from outside

While pova is open in a terminal, you can drive it from anywhere else: a
keyboard shortcut, a script or a status bar. Run `pova` followed by a command:

| Command | Does |
|---|---|
| `pova toggle` | play / pause |
| `pova play`, `pova pause` | resume, pause |
| `pova next`, `pova prev` | next track, previous track (or restart the current one) |
| `pova like` | like or unlike the current song (needs you to be signed in) |
| `pova dislike` | dislike the current song and skip it (needs you to be signed in) |
| `pova volume 40`, `pova volume +5`, `pova volume -5` | set or change the volume |
| `pova seek 90`, `pova seek +10`, `pova seek -10` | jump to a time, or skip in seconds |
| `pova add <song>` | search and add the top result to the end of the queue |
| `pova play-next <song>` | search and play the top result after the current song |
| `pova play-now <song>` | search and play the top result right away |
| `pova status` | print what's playing (see below) |

The search commands take plain words, no quotes needed:
`pova add seether fine again`.

If pova isn't running, or a command can't be done (the queue is empty, say),
it prints why and exits with status 1, so scripts can tell.

## Keyboard shortcuts

Bind the commands to keys in your desktop or window manager. For example:

**KDE Plasma:** System Settings → Keyboard → Shortcuts → Add New → Command
or Script, then enter `pova toggle` and pick a key.

**Hyprland** (`~/.config/hypr/hyprland.conf`):

```
bind = SUPER, P, exec, pova toggle
bind = SUPER, bracketright, exec, pova next
bind = SUPER, bracketleft, exec, pova prev
```

**sway / i3** (`~/.config/sway/config`):

```
bindsym $mod+p exec pova toggle
bindsym $mod+bracketright exec pova next
```

Media keys and desktop media widgets work by themselves (next section);
these commands add search, the queue and likes on top.

## Media keys and desktop widgets

pova shows up as a media player on your desktop, like a browser playing
audio: the media widget in KDE's panel, GNOME's top bar and lock screen,
Cinnamon, XFCE and others show the song and cover, and play / pause, next,
previous, seeking, volume, shuffle and repeat work from there. So do media
keys, KDE Connect and GSConnect on your phone, and waybar's `mpris` module.
It's the freedesktop MPRIS standard, built into pova (no extra program); turn
it off with "Media controls" in the `esc` menu.

On a tiling window manager without a media-key daemon, bind the keys to
[playerctl](https://github.com/altdesktop/playerctl):

```
# sway / i3
bindsym XF86AudioPlay exec playerctl -p pova play-pause
bindsym XF86AudioNext exec playerctl -p pova next
bindsym XF86AudioPrev exec playerctl -p pova previous

# Hyprland
bind = , XF86AudioPlay, exec, playerctl -p pova play-pause
bind = , XF86AudioNext, exec, playerctl -p pova next
bind = , XF86AudioPrev, exec, playerctl -p pova previous
```

If you have mpv-mpris installed, pova keeps it out of its own mpv while media
controls are on, so you don't see two players; your other mpv windows still
use it. This is decided when pova starts, so after switching media controls
in the menu, restart pova once. If your mpv.conf or `mpv_args` already choose
which scripts load (`load-scripts`, `scripts`), pova leaves that alone.

Over ssh or on a text console there's no desktop session to talk to, so media
controls simply stay quiet there.

## Status bars

`pova status` prints one line, `{artist} - {title}` by default, and nothing
when no song is loaded. Choose your own format with `-f`:

```sh
pova status -f '{state}: {artist} - {title} ({position}/{duration}) {liked}'
# playing: Seether - Fine Again (1:03/4:03) ♥
```

Placeholders: `{title}` `{artist}` `{album}` `{state}` (playing or paused)
`{position}` `{duration}` `{volume}` `{liked}` (♥ or nothing).

**waybar** (`~/.config/waybar/config`):

```json
"custom/pova": {
    "exec": "pova status 2>/dev/null",
    "interval": 2,
    "on-click": "pova toggle",
    "on-scroll-up": "pova next",
    "on-scroll-down": "pova prev"
}
```

**polybar:**

```ini
[module/pova]
type = custom/script
exec = pova status 2>/dev/null
interval = 2
click-left = pova toggle
```

## How it works

pova listens on a Unix socket at `$XDG_RUNTIME_DIR/pova/control.sock`, which
only your user can reach. If you open a second pova, the first one keeps the
socket. Each command is one line of text, and the answer is one line starting
with `ok` or `error`.

Back to the [README](../README.md).

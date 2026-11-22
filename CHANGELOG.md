# Changelog

## 1.0.1 — packages

- **.deb and .rpm packages.** `sudo dnf install ./pova-*.rpm` or
  `sudo apt install ./pova_*.deb` installs pova with mpv, yt-dlp and the
  PipeWire tools from your distro's repositories, and adds it to the app menu.
- **AppImage** for every release.
- The sign-in helper is installed as the `pova-import-cookie` command.
- Every package is test-installed on clean Fedora and Ubuntu systems before
  it's published.

## 1.0.0 — first release

pova plays YouTube Music in your terminal, looks like btop, and has a real
cava visualizer.

**Playing music**
- Search songs, videos, albums, artists and playlists, with suggestions as you
  type and typo correction ("call me when your sober" still finds it).
- Queue songs (play next or add to the end), reorder and remove them; autoplay
  keeps going with a radio of similar songs.
- Gapless playback, and songs start in about a third of a second because pova
  looks up the next ones ahead of time.
- Time-synced lyrics from LRCLIB, falling back to YouTube Music's lyrics.
- Sign in with one command (`scripts/import-cookie.sh`) for your library,
  liked songs, history and likes; songs you've liked show a ♥ everywhere.

**Looks**
- A Rust port of cava's `cavacore`, listening only to pova's own audio; four
  modes (bars, mirror, mono, wave).
- Real album art in kitty; a halfblock version elsewhere.
- Every btop theme, with a live picker, and a btop-style Esc menu.
- Mouse support.

**Under the hood**
- Clean shutdown: mpv and the audio capture always exit with pova.
- See [docs/how-it-works.md](docs/how-it-works.md) for the architecture and
  known limits.

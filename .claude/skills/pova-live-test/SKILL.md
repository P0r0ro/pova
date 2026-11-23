---
name: pova-live-test
description: Run a real pova instance in an isolated tmux session to check UI changes, errors, resume, or remote-control commands without touching the maintainer's config, cookie, socket or running player. Use after changing pova's UI or behaviour and before saying a feature works.
---

# Live-testing pova safely

`harness.sh` (next to this file) starts pova in a private tmux server with its
own XDG config / state / cache / runtime dirs and volume 0, so nothing touches
the maintainer's real setup. Unit tests in `src/app.rs` (`playlist_tests`,
`resolve_tests`) cover logic; use this to see the real thing.

## Use

```sh
cargo build                               # the harness runs target/debug/pova
. .claude/skills/pova-live-test/harness.sh
pt_start                                  # optional: pt_start '--play "seether fine again"'
sleep 8; pt_snap | tail -15               # screen as text
pt_k "q"; pt_key Escape                   # type text / press a named key
pt_P status                               # a remote command against this pova only
pt_log | tail                             # its log
pt_stop                                   # quit pova and the tmux server
pt_cleanup                                # remove every temp dir it made
```

`pt_fake_cookie` before `pt_start` signs in with an invalid cookie, to see the
"signed out" error path. There is no way to test real sign-in here, and that
is intentional: ask the maintainer to try features that need their account
(likes, playlists).

## Gotchas this handles

- **Socket paths max out around 108 bytes**, so the runtime dir is a short
  `/tmp/pvrt.XXXX`, not the session scratchpad.
- Overriding `XDG_RUNTIME_DIR` hides PipeWire's socket from mpv ("audio output
  initialization failed"), so `PIPEWIRE_RUNTIME_DIR` / `PULSE_RUNTIME_PATH`
  point at the real `/run/user/$UID`.
- Volume is 0 and album art is off (halfblocks would just be noise in text).
- `pt_P` sets the same env as the TUI, so remote commands never reach the
  maintainer's own pova.

Each tool call can be a new shell: just source `harness.sh` again. It
remembers the session in `/tmp/pova-live.state` and reuses the same pova.
To test a different build, set `PT_BIN` before the first source.

Always run `pt_stop` and `pt_cleanup` when done.

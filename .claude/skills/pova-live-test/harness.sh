# Source me: isolated live-test rig for pova (see SKILL.md).
# Everything lives under one temp dir plus a short runtime dir; nothing
# touches the real ~/.config/pova, cookie, state, socket or running player.

PT_ROOT=$(git rev-parse --show-toplevel 2>/dev/null || pwd)
PT_BIN=${PT_BIN:-$PT_ROOT/target/debug/pova}
PT_X=${PT_X:-$(mktemp -d /tmp/pova-live.XXXX)}
# Unix socket paths are limited to ~108 bytes: keep this one short.
PT_RT=${PT_RT:-$(mktemp -d /tmp/pvrt.XXXX)}
chmod 700 "$PT_RT"
PT_TMUX="tmux -L pova-live-$$"
mkdir -p "$PT_X/config/pova" "$PT_X/state" "$PT_X/cache" "$PT_X/data"
cat > "$PT_X/config/pova/config.toml" <<'CFG'
theme = "Default"
volume = 0
album_art = false
CFG

pt_env() {
  env XDG_CONFIG_HOME="$PT_X/config" XDG_STATE_HOME="$PT_X/state" \
      XDG_CACHE_HOME="$PT_X/cache" XDG_DATA_HOME="$PT_X/data" \
      XDG_RUNTIME_DIR="$PT_RT" \
      PIPEWIRE_RUNTIME_DIR="/run/user/$(id -u)" PULSE_RUNTIME_PATH="/run/user/$(id -u)/pulse" \
      "$@"
}

# pt_start [pova args] [width] [height]
pt_start() {
  local uid; uid=$(id -u)
  $PT_TMUX kill-server 2>/dev/null
  $PT_TMUX -f /dev/null new-session -d -s t -x "${2:-140}" -y "${3:-40}" \
    "env XDG_CONFIG_HOME=$PT_X/config XDG_STATE_HOME=$PT_X/state XDG_CACHE_HOME=$PT_X/cache \
         XDG_DATA_HOME=$PT_X/data XDG_RUNTIME_DIR=$PT_RT \
         PIPEWIRE_RUNTIME_DIR=/run/user/$uid PULSE_RUNTIME_PATH=/run/user/$uid/pulse \
         TERM=xterm-256color $PT_BIN ${1:-}; sleep 2"
}
pt_k() { $PT_TMUX send-keys -t t -l "$1"; }            # type literal text
pt_key() { $PT_TMUX send-keys -t t "$@"; }             # named keys: Escape Enter Down
pt_snap() { $PT_TMUX capture-pane -t t -p; }           # screen as plain text
pt_P() { pt_env "$PT_BIN" "$@"; }                      # remote command to this pova
pt_log() { cat "$PT_X/cache/pova/pova.log" 2>/dev/null; }
pt_fake_cookie() {
  printf 'SAPISID=fake/AbCd; __Secure-3PAPISID=fake/AbCd; SID=fake; HSID=fake; SSID=fake; APISID=fake/ab\n' \
    > "$PT_X/config/pova/cookie.txt"
}
pt_stop() { pt_k Q 2>/dev/null; sleep 1; $PT_TMUX kill-server 2>/dev/null; }
pt_cleanup() {
  pt_stop
  rm -rf "$PT_X" "$PT_RT"
  rm -f "${TMUX_TMPDIR:-/tmp}/tmux-$(id -u)/pova-live-$$"   # the private tmux socket
}

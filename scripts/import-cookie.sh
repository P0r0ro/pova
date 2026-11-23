#!/usr/bin/env bash
# Sign pova in to YouTube Music by copying your browser's cookies.
#
#   ./scripts/import-cookie.sh chrome            # or firefox, chromium, brave, edge, ...
#   ./scripts/import-cookie.sh "chrome:Profile 2" # a specific browser profile
#
# yt-dlp reads (and decrypts) the browser's cookie store; this script keeps
# only the youtube.com cookies, joins them into one Cookie header and writes
# it to ~/.config/pova/cookie.txt (mode 600). Cookie values are never printed.
# Override the destination with POVA_COOKIE_OUT=/some/file.
set -euo pipefail

browser="${1:-}"
if [[ -z "$browser" ]]; then
    echo "usage: $0 <browser>[:profile]   e.g. $0 chrome" >&2
    exit 2
fi
command -v yt-dlp >/dev/null || { echo "yt-dlp is not installed" >&2; exit 1; }

out="${POVA_COOKIE_OUT:-${XDG_CONFIG_HOME:-$HOME/.config}/pova/cookie.txt}"
mkdir -p "$(dirname "$out")"
umask 077
work="$(mktemp -d)"
trap 'shred -u "$work"/jar 2>/dev/null || rm -f "$work"/jar; rmdir "$work" 2>/dev/null || true' EXIT

js=()
if ! command -v deno >/dev/null && command -v node >/dev/null; then
    js=(--js-runtimes node)
fi

# --skip-download still makes yt-dlp load (and save) the cookie jar. A format
# error at the end is harmless: we only need the cookies.
yt-dlp -q --no-warnings "${js[@]}" --cookies-from-browser "$browser" --cookies "$work/jar" \
    --skip-download "https://music.youtube.com/watch?v=dQw4w9WgXcQ" >/dev/null 2>&1 || true
[[ -s "$work/jar" ]] || { echo "could not read cookies from '$browser'" >&2; exit 1; }

python3 -I - "$work/jar" "$out" <<'PY'
import sys
src, dst = sys.argv[1], sys.argv[2]
jar = {}
for line in open(src, encoding="utf-8", errors="replace"):
    if line.startswith("#HttpOnly_"):
        line = line[len("#HttpOnly_"):]
    if not line.strip() or line.startswith("#"):
        continue
    f = line.rstrip("\n").split("\t")
    if len(f) < 7:
        continue
    domain, name, value = f[0].lstrip("."), f[5], f[6]
    if domain == "youtube.com" or domain.endswith(".youtube.com"):
        if name not in jar or domain == "music.youtube.com":
            jar[name] = value
if not any(k in jar for k in ("SAPISID", "__Secure-3PAPISID")):
    sys.exit("no YouTube login cookies found - are you signed in to music.youtube.com in that browser?")
open(dst, "w").write("; ".join(f"{k}={v}" for k, v in jar.items()) + "\n")
print(f"wrote {len(jar)} cookies to {dst}")
PY
chmod 600 "$out"
echo "done - pova picks it up by itself within a few seconds (older versions: restart pova); the player box should say 'signed in'"

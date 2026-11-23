#!/usr/bin/env bash
# Install a built pova package on a clean distro container and check that it
# works: pova runs, its runtime dependencies (mpv, yt-dlp, the PipeWire tools)
# came along from the distro's own repositories, and the extras are in place.
#
#   scripts/test-packages.sh rpm pova-1.1.0-1.x86_64.rpm fedora:latest
#   scripts/test-packages.sh deb pova_1.1.0-1_amd64.deb  ubuntu:24.04
#   scripts/test-packages.sh bin target/release/pova     archlinux:latest
#
# "bin" installs the dependencies the AUR package declares, then runs the
# plain binary (the AUR package ships that same binary).
# Used by .github/workflows/release.yml and packages.yml. Needs docker
# (or set DOCKER=podman).
set -euo pipefail

kind=${1:?kind: rpm, deb or bin}
file=${2:?package or binary file}
image=${3:?container image, e.g. fedora:latest}
[ -f "$file" ] || { echo "no such file: $file" >&2; exit 1; }

dir=$(cd "$(dirname "$file")" && pwd)
name=$(basename "$file")

case "$kind" in
  rpm)
    script='dnf install -y -q "/w/$NAME"'
    checks="pova-import-cookie"
    ;;
  deb)
    script='export DEBIAN_FRONTEND=noninteractive
            apt-get update -qq
            apt-get install -y -qq "/w/$NAME" > /dev/null'
    checks="pova-import-cookie"
    ;;
  bin)
    script='pacman -Syu --noconfirm --needed mpv yt-dlp pipewire-audio > /dev/null
            install -m755 "/w/$NAME" /usr/local/bin/pova'
    checks=""
    ;;
  *) echo "kind must be rpm, deb or bin" >&2; exit 1 ;;
esac

echo "== $kind on $image: $name"
"${DOCKER:-docker}" run --rm -v "$dir:/w:ro,z" -e NAME="$name" -e CHECKS="$checks" -e KIND="$kind" "$image" bash -euc "
  $script
  pova --version
  for c in mpv yt-dlp pw-record pw-dump \$CHECKS; do command -v \"\$c\"; done
  if [ \"\$KIND\" != bin ]; then test -f /usr/share/applications/pova.desktop; fi
"
echo "== ok: $kind on $image"

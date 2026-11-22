#!/usr/bin/env bash
# Build pova's source RPM: the git tree plus a tarball of vendored crates, so
# the RPM itself builds offline (as COPR and Fedora builders require).
# Usage: packaging/copr/make-srpm.sh OUTDIR
set -euo pipefail
outdir=$(realpath "${1:-.}")
top=$(git rev-parse --show-toplevel)
version=$(sed -n 's/^version = "\(.*\)"/\1/p' "$top/Cargo.toml" | head -1)
work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT

git -C "$top" archive --format=tar.gz --prefix="pova-$version/" -o "$work/pova-$version.tar.gz" HEAD
(cd "$top" && cargo vendor --locked --versioned-dirs "$work/vendor" > /dev/null)
tar -C "$work" -czf "$work/pova-$version-vendor.tar.gz" vendor
sed "s/^Version:.*/Version:        $version/" "$top/packaging/copr/pova.spec" > "$work/pova.spec"
rpmbuild -bs --define "_sourcedir $work" --define "_srcrpmdir $outdir" "$work/pova.spec"

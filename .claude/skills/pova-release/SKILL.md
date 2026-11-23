---
name: pova-release
description: Release a new pova version to every channel (GitHub release with .rpm/.deb/AppImage/tarball, crates.io, AUR checksums) from one tag. Use when the maintainer asks to release, publish, ship or tag a version of pova.
---

# Releasing pova

Every format is built from one tagged commit by `.github/workflows/release.yml`,
which also refuses a tag that doesn't match `Cargo.toml`. `main` is protected,
so version changes go through PRs. **Tagging publishes: get the maintainer's
OK before step 6.**

## Steps

1. **Bump** on a new branch `release-X.Y.Z`:
   `scripts/release.sh bump X.Y.Z`. It updates Cargo.toml/lock, the README's
   install commands, the COPR spec and the AUR package, then runs `check`.
2. **CHANGELOG.md**: add `## X.Y.Z — <short theme>` at the top (rename an
   "Unreleased" heading if there is one). User-facing wording, no emojis.
3. **PR**: commit, push, `gh pr create`. Wait for `check`, `msrv`, `deny`.
4. **Rehearse** every format without publishing:
   `gh workflow run release.yml --ref release-X.Y.Z -f tag=release-X.Y.Z -f publish=false`
   then `gh run watch <id> --exit-status`. All steps up to "Test the .deb on
   Ubuntu" must pass; Upload and crates.io are skipped.
5. **Merge** the PR (squash) after the maintainer's OK, then
   `git checkout main && git pull --ff-only` and `scripts/release.sh check`.
6. **Tag** (after the maintainer's OK):
   `git tag -a vX.Y.Z -m "pova X.Y.Z" && git push origin vX.Y.Z`.
   Right after, create the release page with the CHANGELOG section as notes
   (`gh release create vX.Y.Z --verify-tag --title ... --notes-file ...`);
   the workflow waits up to a minute for it before falling back to
   auto-generated notes.
7. **Verify** once the run is green:
   - `gh release view vX.Y.Z --json assets` lists 8 files (rpm, deb, AppImage,
     tar.gz, each with `.sha256`);
   - download one and `sha256sum -c` it; `rpm -qp --qf '%{VERSION}'` matches;
   - crates.io: `curl -s -A pova-release https://crates.io/api/v1/crates/pova`
     shows `max_version` X.Y.Z (the workflow publishes it with the
     `CARGO_REGISTRY_TOKEN` secret; if it printed "No CARGO_REGISTRY_TOKEN",
     tell the maintainer).
8. **AUR checksums** on a branch: `scripts/release.sh aur`, commit, PR, merge.
9. Tell the maintainer what was published and where; update the local
   install if they use one (`cargo install --path . --locked --force`).

## If something fails

- Version guard failed: the tag and Cargo.toml disagree. Delete the tag
  (`git push origin :refs/tags/vX.Y.Z`, `git tag -d vX.Y.Z`) only with the
  maintainer's OK, fix via PR, re-tag.
- A package test failed: reproduce with
  `DOCKER=podman scripts/test-packages.sh <rpm|deb|bin> <file> <image>`.
- Upload "release not found": re-run the job (`gh run rerun <id> --failed`).

# Development

```sh
./scripts/check.sh                      # rustfmt check, clippy (warnings are errors), all tests
cargo test -- --ignored live_api        # also hits the real YouTube Music API
```

The tests cover the cavacore port with real test tones, every btop theme on
your machine, the queue, the search box, likes, the fast-start resolver, kitty
image output, and rendering at five terminal sizes.
[MILESTONES.md](../MILESTONES.md) shows how the project came together.

The architecture is described in [how-it-works.md](how-it-works.md).

## CI and pull requests

`main` is protected: every change goes in through a pull request, squash
merged once the required checks pass. They live in `.github/workflows/`:

| Workflow | When | What |
|---|---|---|
| `ci.yml` | every PR and push to main | `check` (scripts/check.sh), `msrv` (oldest supported Rust), `deny` (advisories, licences, sources via `deny.toml`) |
| `packages.yml` | PRs touching packaging, weekly, by hand | installs the .rpm / .deb / binary on Fedora, Ubuntu, Debian and Arch; rebuilds the COPR source RPM |
| `release.yml` | version tags, or by hand for a rehearsal | builds and tests every release format, uploads them, publishes to crates.io |

Dependabot opens weekly dependency PRs, which go through the same checks.
`scripts/test-packages.sh` runs a package install test locally too
(`DOCKER=podman scripts/test-packages.sh rpm <file> fedora:latest`), and
`cargo deny check` runs the dependency check (`cargo install cargo-deny`).

## Releasing

`scripts/release.sh bump X.Y.Z` puts the new version everywhere it appears
(Cargo.toml, the README's install commands, the COPR spec, the AUR package)
and prints the remaining steps: a PR with the CHANGELOG entry, a rehearsal
run, merge, then pushing the `vX.Y.Z` tag, which builds, tests and publishes
every format from that one commit. `release.yml` refuses a tag that doesn't
match the version in Cargo.toml. Afterwards `scripts/release.sh aur` fills in
the AUR checksums (in its own PR), and `scripts/release.sh check` confirms
every channel agrees.

Back to the [README](../README.md).

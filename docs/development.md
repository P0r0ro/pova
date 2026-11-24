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
| `apt.yml` | after every successful release build, or by hand | rebuilds the signed apt repository on GitHub Pages from the last three releases' .deb, tests `apt install pova` from it on Debian and Ubuntu, and deploys it |

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
match the version in Cargo.toml. Afterwards `scripts/release.sh copr` builds the
tag on [Fedora COPR](https://copr.fedorainfracloud.org/coprs/p0r0ro/pova/),
`scripts/release.sh aur` fills in the AUR checksums (in its own PR), and
`scripts/release.sh check` confirms every channel agrees. The apt repository
updates by itself once the release build finishes.

### The apt repository

`apt.yml` publishes `https://p0r0ro.github.io/pova/apt` (GitHub Pages,
source: GitHub Actions) with `scripts/apt-repo.sh`. It copies each release's
.deb from the release page after checking its `.sha256`, so apt serves the
same binary as every other channel. The repository is signed with pova's own
key, which lives only in the `APT_SIGNING_KEY` secret (armored private key;
`APT_SIGNING_PASSPHRASE` if it has one). Its public half is published next to
the repository as `pova.gpg` / `pova.asc`.

To test locally (needs `apt-ftparchive`, so run the build in a Debian
container on other distros):

```sh
scripts/apt-repo.sh fetch debs 2                    # download and verify .deb files
scripts/apt-repo.sh build debs site                 # throwaway key without APT_SIGNING_KEY
DOCKER=podman scripts/apt-repo.sh test site debian:stable
```

Back to the [README](../README.md).

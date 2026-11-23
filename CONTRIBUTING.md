# Contributing to pova

Thanks for helping. pova is a small project, so the process is light.

## Before you start

- pova is **Linux only**: it needs PipeWire and mpv, and it's tested on
  Fedora, Ubuntu, Debian and Arch. Changes for other systems are out of scope.
- For anything bigger than a fix, open an issue first so we can agree on the
  approach before you spend time on it.

## Build and run

```sh
git clone https://github.com/P0r0ro/pova
cd pova
cargo run --release
```

You need a Rust toolchain (1.90 or newer) plus `mpv`, `yt-dlp` and the
PipeWire tools; the [README](README.md#install) lists the packages per
distro. [docs/how-it-works.md](docs/how-it-works.md) explains the
architecture.

## The one command to run

```sh
scripts/check.sh
```

It checks formatting (`cargo fmt`), runs clippy with warnings as errors, and
runs the test suite. Run `cargo fmt` to fix formatting. If `check.sh` passes
on your machine, the main CI job will pass too: it runs the same script.

## Pull requests

`main` is protected: every change, including the maintainer's, goes in
through a pull request, and it can only be merged when these checks pass:

| Check | What it does |
|---|---|
| `check` | `scripts/check.sh`: format, clippy, tests |
| `msrv` | builds with the oldest supported Rust (`rust-version` in Cargo.toml) |
| `deny` | security advisories, dependency licences and sources (`deny.toml`) |

PRs that touch packaging also run **package tests**: the `.rpm`, `.deb` and
binary are installed on clean Fedora, Ubuntu, Debian and Arch containers, and
the COPR source package is rebuilt.

Please:

- keep a PR to one change, with a short description of what and why;
- add or update tests for behaviour changes;
- update the docs for user-visible changes: [CHANGELOG.md](CHANGELOG.md)
  (under a new "Unreleased" heading if there isn't one), and
  [docs/keys.md](docs/keys.md) or [docs/configuration.md](docs/configuration.md)
  when keys or options change;
- never commit your `cookie.txt`, tokens or logs with personal data.

PRs are squash-merged, so your branch's history doesn't need to be tidy.

## Reporting bugs and security issues

Bugs: open an issue with the bug report form. Security problems: please
report them privately, see [SECURITY.md](SECURITY.md).

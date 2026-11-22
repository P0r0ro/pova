# Fedora COPR build of pova, from source with vendored crates (offline build).
# The source tarballs are made by packaging/copr/make-srpm.sh (see .copr/Makefile).

# The release profile strips the binary, so there is no debuginfo to package.
%global debug_package %{nil}

Name:           pova
Version:        1.0.1
Release:        1%{?dist}
Summary:        YouTube Music in the terminal, with a cava visualizer and btop themes
License:        MIT
URL:            https://github.com/P0r0ro/pova
Source0:        pova-%{version}.tar.gz
Source1:        pova-%{version}-vendor.tar.gz

ExclusiveArch:  x86_64 aarch64
BuildRequires:  rust >= 1.88
BuildRequires:  cargo

Requires:       mpv
Requires:       yt-dlp
Requires:       pipewire-utils
Recommends:     nodejs
Recommends:     python3
Recommends:     mpv-mpris
Recommends:     btop

%description
pova plays YouTube Music in the terminal with a btop-style layout, a
visualizer ported from cava that listens only to pova's own audio, synced
lyrics, album art, a queue, likes and every btop theme.

%prep
%autosetup -n pova-%{version}
tar --no-same-owner -xzf %{SOURCE1}
mkdir -p .cargo
cat > .cargo/config.toml <<'CFG'
[source.crates-io]
replace-with = "vendored-sources"

[source.vendored-sources]
directory = "vendor"
CFG

%build
cargo build --release --locked --offline

%install
install -Dm755 target/release/pova %{buildroot}%{_bindir}/pova
install -Dm755 scripts/import-cookie.sh %{buildroot}%{_bindir}/pova-import-cookie
install -Dm644 packaging/appimage/pova.desktop %{buildroot}%{_datadir}/applications/pova.desktop
install -Dm644 packaging/appimage/pova.png %{buildroot}%{_datadir}/icons/hicolor/512x512/apps/pova.png

%files
%license LICENSE THIRD_PARTY_NOTICES.md
%doc README.md CHANGELOG.md
%{_bindir}/pova
%{_bindir}/pova-import-cookie
%{_datadir}/applications/pova.desktop
%{_datadir}/icons/hicolor/512x512/apps/pova.png

%changelog
* Sun Nov 22 2026 P0r0ro <250746769+P0r0ro@users.noreply.github.com> - 1.0.1-1
- First COPR build

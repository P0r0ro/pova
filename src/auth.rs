//! Signing in to YouTube Music without manual DevTools copying.
//!
//! Uses `yt-dlp --cookies-from-browser` to read the browser's existing
//! `music.youtube.com` session, keeps only the `youtube.com` cookies, and
//! writes them atomically (mode `0600`) to `~/.config/pova/cookie.txt`.
//! Cookie values are never logged or printed.

use std::collections::HashMap;
use std::fs::OpenOptions;
use std::io::Write;
use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use anyhow::Context;
use tokio::process::Command;

use crate::player::mpv::which;

/// Generous timeout so desktop keyrings (KWallet / GNOME Keyring) have time
/// to prompt the user if locked.
const EXTRACT_TIMEOUT: Duration = Duration::from_secs(30);

/// Fallback URL used only if an older `yt-dlp` refuses to dump cookies without
/// a URL argument.
const FALLBACK_URL: &str = "https://music.youtube.com/watch?v=dQw4w9WgXcQ";

/// Page opened in the user's browser when they aren't signed in yet.
pub const YTM_SIGN_IN_URL: &str = "https://music.youtube.com";

static TMP_SEQ: AtomicU64 = AtomicU64::new(0);

/// Why reading cookies from a browser didn't immediately sign in.
#[derive(Debug)]
pub enum ExtractError {
    /// Browser cookies were readable, but there's no YouTube Music login yet.
    NotSignedIn { spec: String },
    /// Hard failure (missing yt-dlp, unknown browser, locked keyring, timeout, etc.).
    Other(String),
}

impl std::fmt::Display for ExtractError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ExtractError::NotSignedIn { spec } => write!(
                f,
                "No YouTube Music login found in {spec}. Open {YTM_SIGN_IN_URL} in that browser, sign in, and try again."
            ),
            ExtractError::Other(msg) => f.write_str(msg),
        }
    }
}

impl std::error::Error for ExtractError {}

/// Open `https://music.youtube.com` in the browser (and Chromium profile, if
/// given) matching `spec`, or the system default browser when `spec` is empty.
/// Detached with null stdio so the TUI is never disturbed.
pub fn open_in_browser(spec: &str) -> bool {
    let trimmed = spec.trim();
    let (head, profile) = match trimmed.split_once(':') {
        Some((h, p)) => (h, Some(p.split("::").next().unwrap_or(p).trim())),
        None => (trimmed, None),
    };
    let browser_id = head.split('+').next().unwrap_or("").to_ascii_lowercase();
    let profile_lower = profile.unwrap_or("").to_ascii_lowercase();
    let (bins, chromium): (&[&str], bool) = if profile_lower.contains("brave-origin") {
        (&["brave-origin", "brave-origin-stable", "brave-browser", "brave"], true)
    } else if profile_lower.contains("brave-browser-beta") {
        (&["brave-browser-beta", "brave-browser"], true)
    } else if profile_lower.contains("brave-browser-nightly") {
        (&["brave-browser-nightly", "brave-browser"], true)
    } else if profile_lower.contains("librewolf") {
        (&["librewolf"], false)
    } else if profile_lower.contains("zen") {
        (&["zen-browser", "zen"], false)
    } else if profile_lower.contains("floorp") {
        (&["floorp"], false)
    } else if profile_lower.contains("waterfox") {
        (&["waterfox"], false)
    } else {
        match browser_id.as_str() {
            "firefox" => (&["firefox"], false),
            "chrome" => (&["google-chrome-stable", "google-chrome", "chrome"], true),
            "chromium" => (&["chromium", "chromium-browser"], true),
            "brave" => (&["brave-browser", "brave", "brave-origin"], true),
            "edge" => (&["microsoft-edge-stable", "microsoft-edge"], true),
            "vivaldi" => (&["vivaldi-stable", "vivaldi"], true),
            "opera" => (&["opera"], false),
            _ => (&[], false),
        }
    };
    let profile_dir_name = profile.and_then(|p| {
        if p.contains('/') {
            Path::new(p).file_name().and_then(|s| s.to_str())
        } else {
            Some(p)
        }
    });
    for bin in bins {
        if which(bin).is_some() {
            let mut cmd = std::process::Command::new(bin);
            if chromium
                && let Some(prof) = profile_dir_name
                && !prof.is_empty()
                && !prof.starts_with('-')
            {
                cmd.arg(format!("--profile-directory={prof}"));
            }
            if cmd
                .arg(YTM_SIGN_IN_URL)
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .spawn()
                .is_ok()
            {
                return true;
            }
        }
    }
    if which("xdg-open").is_some() {
        return std::process::Command::new("xdg-open")
            .arg(YTM_SIGN_IN_URL)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .is_ok();
    }
    false
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BrowserProfile {
    /// Spec passed to `yt-dlp --cookies-from-browser`, e.g. `"brave:/home/.../Default"`.
    pub spec: String,
    /// Human-readable label shown in the UI, e.g. `"Brave Origin — Personal"`.
    pub label: String,
    /// True if this is the desktop's default browser (and its default/last-used profile).
    pub default: bool,
}

struct ChromiumVariant {
    /// Identifier matched against `mimeapps.list` default browser detection.
    key: &'static str,
    /// Browser name passed to `yt-dlp --cookies-from-browser`.
    ytdlp_id: &'static str,
    /// Display name shown in the UI.
    name: &'static str,
    /// Whether this browser stores multiple profiles in subdirectories.
    supports_profiles: bool,
    /// Relative paths under `$XDG_CONFIG_HOME`.
    config_dirs: &'static [&'static str],
    /// Relative paths under `$HOME` (Flatpak / Snap).
    home_dirs: &'static [&'static str],
}

const CHROMIUM_VARIANTS: &[ChromiumVariant] = &[
    ChromiumVariant {
        key: "brave-origin",
        ytdlp_id: "brave",
        name: "Brave Origin",
        supports_profiles: true,
        config_dirs: &["BraveSoftware/Brave-Origin"],
        home_dirs: &[".var/app/com.brave.Origin/config/BraveSoftware/Brave-Origin"],
    },
    ChromiumVariant {
        key: "brave",
        ytdlp_id: "brave",
        name: "Brave",
        supports_profiles: true,
        config_dirs: &["BraveSoftware/Brave-Browser"],
        home_dirs: &[
            ".var/app/com.brave.Browser/config/BraveSoftware/Brave-Browser",
            "snap/brave/current/.config/BraveSoftware/Brave-Browser",
        ],
    },
    ChromiumVariant {
        key: "brave-beta",
        ytdlp_id: "brave",
        name: "Brave Beta",
        supports_profiles: true,
        config_dirs: &["BraveSoftware/Brave-Browser-Beta"],
        home_dirs: &[],
    },
    ChromiumVariant {
        key: "brave-nightly",
        ytdlp_id: "brave",
        name: "Brave Nightly",
        supports_profiles: true,
        config_dirs: &["BraveSoftware/Brave-Browser-Nightly"],
        home_dirs: &[],
    },
    ChromiumVariant {
        key: "chrome",
        ytdlp_id: "chrome",
        name: "Chrome",
        supports_profiles: true,
        config_dirs: &["google-chrome"],
        home_dirs: &[".var/app/com.google.Chrome/config/google-chrome"],
    },
    ChromiumVariant {
        key: "chrome-beta",
        ytdlp_id: "chrome",
        name: "Chrome Beta",
        supports_profiles: true,
        config_dirs: &["google-chrome-beta"],
        home_dirs: &[],
    },
    ChromiumVariant {
        key: "chrome-unstable",
        ytdlp_id: "chrome",
        name: "Chrome Dev",
        supports_profiles: true,
        config_dirs: &["google-chrome-unstable"],
        home_dirs: &[],
    },
    ChromiumVariant {
        key: "chromium",
        ytdlp_id: "chromium",
        name: "Chromium",
        supports_profiles: true,
        config_dirs: &["chromium"],
        home_dirs: &[
            ".var/app/org.chromium.Chromium/config/chromium",
            "snap/chromium/common/chromium",
        ],
    },
    ChromiumVariant {
        key: "ungoogled-chromium",
        ytdlp_id: "chromium",
        name: "Ungoogled Chromium",
        supports_profiles: true,
        config_dirs: &["ungoogled-chromium"],
        home_dirs: &[".var/app/io.github.ungoogled_software.ungoogled_chromium/config/chromium"],
    },
    ChromiumVariant {
        key: "thorium",
        ytdlp_id: "chromium",
        name: "Thorium",
        supports_profiles: true,
        config_dirs: &["thorium"],
        home_dirs: &[],
    },
    ChromiumVariant {
        key: "edge",
        ytdlp_id: "edge",
        name: "Edge",
        supports_profiles: true,
        config_dirs: &["microsoft-edge"],
        home_dirs: &[".var/app/com.microsoft.Edge/config/microsoft-edge"],
    },
    ChromiumVariant {
        key: "edge-beta",
        ytdlp_id: "edge",
        name: "Edge Beta",
        supports_profiles: true,
        config_dirs: &["microsoft-edge-beta"],
        home_dirs: &[],
    },
    ChromiumVariant {
        key: "vivaldi",
        ytdlp_id: "vivaldi",
        name: "Vivaldi",
        supports_profiles: true,
        config_dirs: &["vivaldi"],
        home_dirs: &[".var/app/com.vivaldi.Vivaldi/config/vivaldi"],
    },
    ChromiumVariant {
        key: "vivaldi-snapshot",
        ytdlp_id: "vivaldi",
        name: "Vivaldi Snapshot",
        supports_profiles: true,
        config_dirs: &["vivaldi-snapshot"],
        home_dirs: &[],
    },
    ChromiumVariant {
        key: "opera",
        ytdlp_id: "opera",
        name: "Opera",
        supports_profiles: false,
        config_dirs: &["opera"],
        home_dirs: &[],
    },
];

struct FirefoxVariant {
    key: &'static str,
    name: &'static str,
    config_dirs: &'static [&'static str],
    home_dirs: &'static [&'static str],
}

const FIREFOX_VARIANTS: &[FirefoxVariant] = &[
    FirefoxVariant {
        key: "firefox",
        name: "Firefox",
        config_dirs: &["mozilla/firefox"],
        home_dirs: &[
            ".mozilla/firefox",
            ".var/app/org.mozilla.firefox/config/mozilla/firefox",
            ".var/app/org.mozilla.firefox/.mozilla/firefox",
            "snap/firefox/common/.mozilla/firefox",
        ],
    },
    FirefoxVariant {
        key: "zen",
        name: "Zen",
        config_dirs: &["zen"],
        home_dirs: &[".zen", ".var/app/app.zen_browser.zen/.zen"],
    },
    FirefoxVariant {
        key: "librewolf",
        name: "LibreWolf",
        config_dirs: &[],
        home_dirs: &[".librewolf", ".var/app/io.gitlab.librewolf-community/.librewolf"],
    },
    FirefoxVariant {
        key: "floorp",
        name: "Floorp",
        config_dirs: &[],
        home_dirs: &[".floorp", ".var/app/one.ablaze.floorp/.floorp"],
    },
    FirefoxVariant {
        key: "waterfox",
        name: "Waterfox",
        config_dirs: &[],
        home_dirs: &[".waterfox", ".var/app/net.waterfox.waterfox/.waterfox"],
    },
];

/// Detect installed Linux browsers and their individual user profiles by reading
/// local browser metadata (`Local State` for Chromium/Brave, `profiles.ini` for
/// Firefox and forks). Fast, offline, and never touches keyrings or cookie DBs.
pub fn detect_profiles() -> Vec<BrowserProfile> {
    let home = std::env::var_os("HOME").map(PathBuf::from);
    let config_home = std::env::var_os("XDG_CONFIG_HOME")
        .filter(|v| !v.is_empty())
        .map(PathBuf::from)
        .or_else(|| home.as_ref().map(|h| h.join(".config")));

    let default_key = detect_default_browser(home.as_deref(), config_home.as_deref());
    let mut found: Vec<(bool, BrowserProfile)> = Vec::new();

    if let (Some(home), Some(cfg)) = (home.as_deref(), config_home.as_deref()) {
        for v in FIREFOX_VARIANTS {
            let is_default_browser = default_key == Some(v.key);
            let roots: Vec<PathBuf> = v
                .config_dirs
                .iter()
                .map(|d| cfg.join(d))
                .chain(v.home_dirs.iter().map(|d| home.join(d)))
                .collect();
            for p in detect_firefox_profiles(&roots, v.name, is_default_browser) {
                found.push((is_default_browser, p));
            }
        }
        for v in CHROMIUM_VARIANTS {
            let is_default_browser = default_key == Some(v.key);
            let dirs: Vec<PathBuf> = v
                .config_dirs
                .iter()
                .map(|d| cfg.join(d))
                .chain(v.home_dirs.iter().map(|d| home.join(d)))
                .filter(|d| d.is_dir())
                .collect();
            for dir in dirs {
                if v.supports_profiles {
                    for p in detect_chromium_profiles(&dir, v.ytdlp_id, v.name, is_default_browser) {
                        found.push((is_default_browser, p));
                    }
                } else if has_chromium_cookies(&dir) {
                    found.push((
                        is_default_browser,
                        BrowserProfile {
                            spec: format!("{}:{}", v.ytdlp_id, dir.display()),
                            label: v.name.to_string(),
                            default: is_default_browser,
                        },
                    ));
                }
            }
        }
    }

    if found.is_empty() {
        let mut fallback = vec![
            BrowserProfile {
                spec: "firefox".into(),
                label: "Firefox".into(),
                default: default_key == Some("firefox"),
            },
            BrowserProfile {
                spec: "brave".into(),
                label: "Brave".into(),
                default: matches!(default_key, Some("brave" | "brave-origin")),
            },
            BrowserProfile {
                spec: "chrome".into(),
                label: "Chrome".into(),
                default: default_key == Some("chrome"),
            },
            BrowserProfile {
                spec: "chromium".into(),
                label: "Chromium".into(),
                default: default_key == Some("chromium"),
            },
            BrowserProfile {
                spec: "edge".into(),
                label: "Edge".into(),
                default: default_key == Some("edge"),
            },
            BrowserProfile {
                spec: "vivaldi".into(),
                label: "Vivaldi".into(),
                default: default_key == Some("vivaldi"),
            },
            BrowserProfile {
                spec: "opera".into(),
                label: "Opera".into(),
                default: default_key == Some("opera"),
            },
        ];
        fallback.sort_by_key(|p| !p.default);
        return fallback;
    }

    // Default browser first, with its active/default profile at index 0.
    found.sort_by_key(|(is_default_browser, p)| (!*is_default_browser, !p.default));
    found.into_iter().map(|(_, p)| p).collect()
}

fn detect_firefox_profiles(roots: &[PathBuf], display_name: &str, is_default_browser: bool) -> Vec<BrowserProfile> {
    let mut entries: Vec<(String, PathBuf, bool)> = Vec::new();

    for root in roots {
        if !root.is_dir() {
            continue;
        }
        if let Ok(ini) = std::fs::read_to_string(root.join("profiles.ini")) {
            for (name, rel_path, is_relative, is_default) in parse_firefox_profiles_ini(&ini) {
                let full = if is_relative {
                    root.join(&rel_path)
                } else {
                    PathBuf::from(&rel_path)
                };
                if full.join("cookies.sqlite").is_file() && !entries.iter().any(|(_, p, _)| *p == full) {
                    entries.push((name, full, is_default));
                }
            }
        }
    }

    if entries.is_empty() {
        return Vec::new();
    }
    if entries.len() == 1 {
        let (_, path, _) = &entries[0];
        return vec![BrowserProfile {
            spec: format!("firefox:{}", path.display()),
            label: display_name.to_string(),
            default: is_default_browser,
        }];
    }
    entries.sort_by(|a, b| (!a.2).cmp(&(!b.2)).then_with(|| a.0.cmp(&b.0)));
    entries
        .into_iter()
        .enumerate()
        .map(|(i, (name, path, is_def))| BrowserProfile {
            spec: format!("firefox:{}", path.display()),
            label: format!("{display_name} — {name}"),
            default: is_default_browser && (is_def || i == 0),
        })
        .collect()
}

fn parse_firefox_profiles_ini(text: &str) -> Vec<(String, String, bool, bool)> {
    let mut out = Vec::new();
    let mut in_profile = false;
    let mut name: Option<String> = None;
    let mut path: Option<String> = None;
    let mut is_relative = true;
    let mut is_default = false;

    let flush = |out: &mut Vec<(String, String, bool, bool)>,
                 name: &mut Option<String>,
                 path: &mut Option<String>,
                 is_relative: &mut bool,
                 is_default: &mut bool| {
        if let (Some(n), Some(p)) = (name.take(), path.take()) {
            out.push((n, p, *is_relative, *is_default));
        }
        *is_relative = true;
        *is_default = false;
    };

    for raw in text.lines() {
        let line = raw.trim();
        if line.starts_with('[') && line.ends_with(']') {
            if in_profile {
                flush(&mut out, &mut name, &mut path, &mut is_relative, &mut is_default);
            }
            let section = &line[1..line.len() - 1];
            in_profile = section.starts_with("Profile");
            continue;
        }
        if !in_profile {
            continue;
        }
        let Some((k, v)) = line.split_once('=') else {
            continue;
        };
        match k.trim() {
            "Name" => name = Some(v.trim().to_string()),
            "Path" => path = Some(v.trim().to_string()),
            "IsRelative" => is_relative = v.trim() != "0",
            "Default" => is_default = v.trim() == "1",
            _ => {}
        }
    }
    if in_profile {
        flush(&mut out, &mut name, &mut path, &mut is_relative, &mut is_default);
    }
    out
}

fn has_chromium_cookies(profile_dir: &Path) -> bool {
    profile_dir.join("Network/Cookies").is_file() || profile_dir.join("Cookies").is_file()
}

fn detect_chromium_profiles(
    browser_dir: &Path,
    id: &str,
    display_name: &str,
    is_default_browser: bool,
) -> Vec<BrowserProfile> {
    let mut entries: Vec<(String, String, bool)> = Vec::new();
    if let Ok(text) = std::fs::read_to_string(browser_dir.join("Local State")) {
        for (dir_name, profile_name, is_last_used) in parse_chromium_local_state(&text) {
            if has_chromium_cookies(&browser_dir.join(&dir_name)) {
                entries.push((dir_name, profile_name, is_last_used));
            }
        }
    }

    if entries.is_empty()
        && let Ok(read_dir) = std::fs::read_dir(browser_dir)
    {
        for entry in read_dir.flatten() {
            let path = entry.path();
            if !path.is_dir() {
                continue;
            }
            let Some(name) = entry.file_name().to_str().map(str::to_string) else {
                continue;
            };
            if matches!(name.as_str(), "Guest Profile" | "System Profile") {
                continue;
            }
            if has_chromium_cookies(&path) {
                let is_def = name == "Default";
                entries.push((name.clone(), name, is_def));
            }
        }
    }

    if entries.is_empty() {
        return Vec::new();
    }
    if entries.len() == 1 {
        let (dir_name, _, _) = &entries[0];
        let full = browser_dir.join(dir_name);
        return vec![BrowserProfile {
            spec: format!("{id}:{}", full.display()),
            label: display_name.to_string(),
            default: is_default_browser,
        }];
    }
    entries.sort_by(|a, b| (!a.2).cmp(&(!b.2)).then_with(|| a.1.cmp(&b.1)));
    entries
        .into_iter()
        .enumerate()
        .map(|(i, (dir_name, profile_name, is_last_used))| {
            let full = browser_dir.join(&dir_name);
            BrowserProfile {
                spec: format!("{id}:{}", full.display()),
                label: format!("{display_name} — {profile_name}"),
                default: is_default_browser && (is_last_used || i == 0),
            }
        })
        .collect()
}

/// Parse Chromium/Brave `Local State` JSON into `(dir_name, display_name, is_last_used)`.
fn parse_chromium_local_state(text: &str) -> Vec<(String, String, bool)> {
    let Ok(json) = serde_json::from_str::<serde_json::Value>(text) else {
        return Vec::new();
    };
    let last_used = json
        .pointer("/profile/last_used")
        .and_then(serde_json::Value::as_str)
        .unwrap_or("");
    let Some(cache) = json.pointer("/profile/info_cache").and_then(serde_json::Value::as_object) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for (dir_name, info) in cache {
        if matches!(dir_name.as_str(), "Guest Profile" | "System Profile") {
            continue;
        }
        let name = info
            .get("name")
            .and_then(serde_json::Value::as_str)
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .unwrap_or(dir_name)
            .to_string();
        out.push((dir_name.clone(), name, dir_name == last_used));
    }
    out
}

fn detect_default_browser(home: Option<&Path>, config_home: Option<&Path>) -> Option<&'static str> {
    let mut candidates = Vec::new();
    if let Some(cfg) = config_home {
        candidates.push(cfg.join("mimeapps.list"));
    }
    if let Some(home) = home {
        candidates.push(home.join(".local/share/applications/mimeapps.list"));
    }
    for path in candidates {
        if let Ok(text) = std::fs::read_to_string(&path)
            && let Some(id) = parse_default_browser_mimeapps(&text)
        {
            return Some(id);
        }
    }
    None
}

fn parse_default_browser_mimeapps(text: &str) -> Option<&'static str> {
    let mut in_default = false;
    for line in text.lines() {
        let line = line.trim();
        if line.starts_with('[') && line.ends_with(']') {
            in_default = line.eq_ignore_ascii_case("[Default Applications]");
            continue;
        }
        if !in_default {
            continue;
        }
        let Some((key, val)) = line.split_once('=') else {
            continue;
        };
        let key = key.trim();
        if !matches!(key, "x-scheme-handler/https" | "x-scheme-handler/http" | "text/html") {
            continue;
        }
        for desktop in val.split(';').map(str::trim).filter(|s| !s.is_empty()) {
            if let Some(id) = desktop_to_browser_id(desktop) {
                return Some(id);
            }
        }
    }
    None
}

fn desktop_to_browser_id(desktop: &str) -> Option<&'static str> {
    let lower = desktop.to_ascii_lowercase();
    if lower.contains("zen") {
        Some("zen")
    } else if lower.contains("librewolf") {
        Some("librewolf")
    } else if lower.contains("floorp") {
        Some("floorp")
    } else if lower.contains("waterfox") {
        Some("waterfox")
    } else if lower.contains("firefox") {
        Some("firefox")
    } else if lower.contains("brave-origin") || lower.contains("com.brave.origin") {
        Some("brave-origin")
    } else if lower.contains("brave-browser-beta") {
        Some("brave-beta")
    } else if lower.contains("brave-browser-nightly") {
        Some("brave-nightly")
    } else if lower.contains("brave") {
        Some("brave")
    } else if lower.contains("vivaldi-snapshot") {
        Some("vivaldi-snapshot")
    } else if lower.contains("vivaldi") {
        Some("vivaldi")
    } else if lower.contains("opera") {
        Some("opera")
    } else if lower.contains("edge-beta") {
        Some("edge-beta")
    } else if lower.contains("edge") {
        Some("edge")
    } else if lower.contains("thorium") {
        Some("thorium")
    } else if lower.contains("ungoogled") {
        Some("ungoogled-chromium")
    } else if lower.contains("chromium") {
        Some("chromium")
    } else if lower.contains("chrome-beta") {
        Some("chrome-beta")
    } else if lower.contains("chrome-unstable") {
        Some("chrome-unstable")
    } else if lower.contains("chrome") {
        Some("chrome")
    } else {
        None
    }
}

/// Validate a user-supplied `yt-dlp --cookies-from-browser` specification
/// (e.g. `chrome`, `firefox:default-release`, `chrome+kwallet:Profile 2`).
pub fn validate_browser_spec(spec: &str) -> anyhow::Result<String> {
    let spec = spec.trim();
    if spec.is_empty() {
        anyhow::bail!("Enter a browser name (for example: firefox, chrome, or chrome:Profile 2).");
    }
    if spec.starts_with('-') {
        anyhow::bail!("Browser name can't start with '-'. Use e.g. firefox or chrome:Profile 2.");
    }
    if spec.chars().any(char::is_control) {
        anyhow::bail!("Browser name contains invalid characters.");
    }
    if spec.chars().count() > 200 {
        anyhow::bail!("Browser specification is too long.");
    }
    Ok(spec.to_string())
}

/// Private temporary directory (`0700`) holding the Netscape cookie jar while
/// `yt-dlp` exports it. Overwrites and deletes the jar file on drop.
struct TempJar {
    dir: PathBuf,
    jar: PathBuf,
}

impl TempJar {
    fn new() -> anyhow::Result<Self> {
        let seq = TMP_SEQ.fetch_add(1, Ordering::Relaxed);
        let dir = std::env::temp_dir().join(format!("pova-auth-{}-{seq}", std::process::id()));
        if dir.exists() {
            let _ = std::fs::remove_dir_all(&dir);
        }
        std::fs::DirBuilder::new()
            .mode(0o700)
            .create(&dir)
            .with_context(|| format!("creating temp directory {}", dir.display()))?;
        let jar = dir.join("jar");
        Ok(Self { dir, jar })
    }
}

impl Drop for TempJar {
    fn drop(&mut self) {
        // Overwrite the temporary jar before unlinking so cookies don't linger.
        if let Ok(meta) = std::fs::metadata(&self.jar) {
            let len = meta.len().min(4 * 1024 * 1024) as usize;
            if let Ok(mut f) = OpenOptions::new().write(true).open(&self.jar) {
                let _ = f.write_all(&vec![0u8; len]);
                let _ = f.flush();
            }
        }
        let _ = std::fs::remove_file(&self.jar);
        let _ = std::fs::remove_dir(&self.dir);
    }
}

/// Extract YouTube Music cookies from `spec` using `yt-dlp`.
/// Returns a formatted `Cookie` header value; never prints or logs cookie contents.
pub async fn extract_from_browser(spec: &str) -> Result<String, ExtractError> {
    let spec = validate_browser_spec(spec).map_err(|e| ExtractError::Other(e.to_string()))?;
    if which("yt-dlp").is_none() {
        return Err(ExtractError::Other(
            "yt-dlp is not installed, so pova can't read browser cookies automatically.".into(),
        ));
    }

    match extract_once(&spec).await {
        Ok(cookie) => Ok(cookie),
        Err(ExtractError::NotSignedIn { .. }) => {
            // Some Chromium forks (e.g. Brave Origin or custom builds) store their
            // encryption key under "Chromium Safe Storage" instead of their parent
            // brand name; retry with `chromium:<path>` if a full profile path was given.
            if let Some((head, path)) = spec.split_once(':')
                && path.starts_with('/')
                && matches!(head.split('+').next(), Some("brave" | "chrome" | "vivaldi" | "edge"))
            {
                let alt = format!("chromium:{path}");
                if let Ok(cookie) = extract_once(&alt).await {
                    return Ok(cookie);
                }
            }
            Err(ExtractError::NotSignedIn { spec })
        }
        Err(e) => Err(e),
    }
}

async fn extract_once(spec: &str) -> Result<String, ExtractError> {
    let tmp = TempJar::new().map_err(|e| ExtractError::Other(e.to_string()))?;

    // Fast path: current yt-dlp opens YoutubeDL and saves the extracted cookie
    // jar on exit without making any network request or solving JS challenges.
    let stderr = run_ytdlp_export(spec, &tmp.jar, false)
        .await
        .map_err(|e| ExtractError::Other(e.to_string()))?;
    let jar_ready = std::fs::metadata(&tmp.jar).is_ok_and(|m| m.len() > 0);

    // Compatibility fallback for older yt-dlp builds that check URL arguments
    // before constructing YoutubeDL.
    let stderr = if !jar_ready && stderr.contains("You must provide at least one URL") && !stderr.contains("ERROR:") {
        run_ytdlp_export(spec, &tmp.jar, true)
            .await
            .map_err(|e| ExtractError::Other(e.to_string()))?
    } else {
        stderr
    };

    let jar_text = match std::fs::read_to_string(&tmp.jar) {
        Ok(s) if !s.trim().is_empty() => s,
        _ => return Err(ExtractError::Other(explain_extract_error(spec, &stderr))),
    };
    parse_netscape_jar(&jar_text, spec)
}

async fn run_ytdlp_export(spec: &str, jar: &Path, with_url: bool) -> anyhow::Result<String> {
    let mut cmd = Command::new("yt-dlp");
    cmd.args(["--no-config", "-q", "--no-warnings", "--cookies-from-browser", spec])
        .arg("--cookies")
        .arg(jar);
    if with_url {
        if which("deno").is_none() && which("node").is_some() {
            cmd.args(["--js-runtimes", "node"]);
        }
        cmd.args(["--skip-download", "--no-playlist", FALLBACK_URL]);
    }
    cmd.stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    // SAFETY: prctl is async-signal-safe; yt-dlp dies with pova.
    unsafe {
        cmd.pre_exec(|| {
            libc::prctl(libc::PR_SET_PDEATHSIG, libc::SIGTERM);
            Ok(())
        });
    }
    let out = tokio::time::timeout(EXTRACT_TIMEOUT, cmd.output())
        .await
        .map_err(|_| {
            anyhow::anyhow!(
                "Reading cookies from {spec} timed out. If a keyring prompt popped up (KWallet / GNOME Keyring), unlock it and try again."
            )
        })?
        .context("running yt-dlp")?;
    Ok(String::from_utf8_lossy(&out.stderr).into_owned())
}

fn explain_extract_error(spec: &str, stderr: &str) -> String {
    let error_line = stderr
        .lines()
        .map(str::trim)
        .find(|l| l.starts_with("ERROR:"))
        .map(|l| l.trim_start_matches("ERROR:").trim());

    if let Some(err) = error_line {
        let lower = err.to_ascii_lowercase();
        if lower.contains("could not find") && lower.contains("cookies database") {
            return format!(
                "Couldn't find a {spec} profile on this machine. Pick another browser, or use \"Other browser / profile\"."
            );
        }
        if lower.contains("unsupported browser") || lower.contains("unknown browser") {
            return format!(
                "yt-dlp doesn't recognize '{spec}'. Try firefox, chrome, chromium, brave, edge, vivaldi, or opera."
            );
        }
        if lower.contains("keyring")
            || lower.contains("kwallet")
            || lower.contains("decrypt")
            || lower.contains("secret")
        {
            return format!(
                "Couldn't unlock {spec}'s encrypted cookies ({err}). Unlock your desktop keyring and try again, or try Firefox."
            );
        }
        if lower.contains("locked") || lower.contains("busy") {
            return format!("Couldn't read {spec}'s cookie database because it's locked. Try again in a moment.");
        }
        return format!("Couldn't read cookies from {spec}: {err}");
    }
    format!("Couldn't read cookies from {spec}. Make sure you're signed in to music.youtube.com in that browser.")
}

/// Parse a Netscape cookie jar and return a single `Cookie` header containing
/// only `youtube.com` / `.youtube.com` cookies.
pub fn parse_netscape_jar(text: &str, source_label: &str) -> Result<String, ExtractError> {
    let mut jar: HashMap<String, String> = HashMap::new();
    let mut order: Vec<String> = Vec::new();

    for raw_line in text.lines() {
        let line = raw_line.strip_prefix("#HttpOnly_").unwrap_or(raw_line);
        let trimmed = line.trim();
        if trimmed.is_empty() || trimmed.starts_with('#') {
            continue;
        }
        let fields: Vec<&str> = line.split('\t').collect();
        if fields.len() < 7 {
            continue;
        }
        let domain = fields[0].trim_start_matches('.');
        let name = fields[5].trim();
        let value = fields[6].trim();
        if name.is_empty() || name.chars().any(char::is_control) || value.chars().any(char::is_control) {
            continue;
        }
        if domain == "youtube.com" || domain.ends_with(".youtube.com") {
            if !jar.contains_key(name) {
                order.push(name.to_string());
                jar.insert(name.to_string(), value.to_string());
            } else if domain == "music.youtube.com" {
                jar.insert(name.to_string(), value.to_string());
            }
        }
    }

    if !jar.contains_key("SAPISID") {
        if let Some(val) = jar.get("__Secure-3PAPISID").cloned() {
            // ytmapi-rs looks specifically for `SAPISID=`; Google sets both
            // SAPISID and __Secure-3PAPISID to the same token.
            order.push("SAPISID".to_string());
            jar.insert("SAPISID".to_string(), val);
        } else {
            return Err(ExtractError::NotSignedIn {
                spec: source_label.to_string(),
            });
        }
    }

    // Put SAPISID / __Secure-3PAPISID first so ytmapi-rs's `split_once("SAPISID=")`
    // + `split_once(';')` always finds a trailing semicolon.
    order.sort_by_key(|k| match k.as_str() {
        "SAPISID" => 0,
        "__Secure-3PAPISID" => 1,
        _ => 2,
    });

    let mut header = order
        .iter()
        .filter_map(|k| jar.get(k).map(|v| format!("{k}={v}")))
        .collect::<Vec<_>>()
        .join("; ");
    if !header.contains(';') {
        header.push(';');
    }
    Ok(header)
}

/// Clean and validate a manually pasted `Cookie` header value.
pub fn normalize_pasted_cookie(raw: &str) -> anyhow::Result<String> {
    let mut s = raw.trim();
    if let Some(rest) = s.strip_prefix("Cookie:").or_else(|| s.strip_prefix("cookie:")) {
        s = rest.trim();
    }
    if s.len() >= 2 && ((s.starts_with('"') && s.ends_with('"')) || (s.starts_with('\'') && s.ends_with('\''))) {
        s = s[1..s.len() - 1].trim();
    }
    if s.is_empty() {
        anyhow::bail!("Paste the Cookie header from music.youtube.com first.");
    }
    if s.chars().any(char::is_control) {
        anyhow::bail!("The pasted cookie contains invalid control characters.");
    }
    let mut out = if let Some((_, after_sapisid)) = s.split_once("SAPISID=") {
        let mut out = s.to_string();
        // ytmapi-rs splits the value after `SAPISID=` on `;`, so ensure one exists.
        if !after_sapisid.contains(';') {
            out.push(';');
        }
        out
    } else if let Some((_, after_3p)) = s.split_once("__Secure-3PAPISID=") {
        let val = after_3p.split(';').next().unwrap_or("").trim();
        if val.is_empty() {
            anyhow::bail!(
                "That doesn't look like a signed-in YouTube Music Cookie header (SAPISID is missing). Copy the full Cookie request header from music.youtube.com."
            );
        }
        format!("SAPISID={val}; {s}")
    } else {
        anyhow::bail!(
            "That doesn't look like a signed-in YouTube Music Cookie header (SAPISID is missing). Copy the full Cookie request header from music.youtube.com."
        );
    };
    if !out.contains(';') {
        out.push(';');
    }
    Ok(out)
}

/// Write `cookie` atomically to `path` with mode `0600` so no other local user
/// can read it and a running pova's watcher never sees a half-written file.
pub fn write_cookie_file(path: &Path, cookie: &str) -> anyhow::Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).with_context(|| format!("creating {}", parent.display()))?;
    }
    let seq = TMP_SEQ.fetch_add(1, Ordering::Relaxed);
    let tmp = path.with_extension(format!("txt.tmp.{}.{seq}", std::process::id()));
    let mut file = OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .mode(0o600)
        .open(&tmp)
        .with_context(|| format!("creating {}", tmp.display()))?;
    std::fs::set_permissions(&tmp, std::fs::Permissions::from_mode(0o600))?;
    file.write_all(cookie.as_bytes())?;
    file.write_all(b"\n")?;
    file.sync_all()?;
    drop(file);
    std::fs::rename(&tmp, path).with_context(|| format!("saving {}", path.display()))?;
    Ok(())
}

/// Remove the saved cookie file. Returns `true` if a file was removed.
pub fn remove_cookie_file(path: &Path) -> anyhow::Result<bool> {
    match std::fs::remove_file(path) {
        Ok(()) => Ok(true),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(e) => Err(e).with_context(|| format!("removing {}", path.display())),
    }
}

/// CLI handler for `pova login [browser]`.
/// If the browser is readable but not signed in to YouTube Music yet, opens
/// `https://music.youtube.com` and waits for the login to appear.
pub async fn login_cli(cookie_file: &Path, browser: Option<&str>) -> anyhow::Result<()> {
    if let Some(spec) = browser {
        match extract_from_browser(spec).await {
            Ok(cookie) => {
                write_cookie_file(cookie_file, &cookie)?;
                println!("Signed in using {spec} (saved to {}).", cookie_file.display());
                println!("If pova is already running, it picks this up automatically.");
                return Ok(());
            }
            Err(ExtractError::NotSignedIn { .. }) => {
                return wait_for_browser_login_cli(cookie_file, spec, spec).await;
            }
            Err(ExtractError::Other(msg)) => anyhow::bail!("{msg}"),
        }
    }

    let profiles = detect_profiles();
    let mut readable_candidate: Option<(String, String)> = None;
    let mut tried = Vec::new();
    for p in &profiles {
        eprint!("Checking {}… ", p.label);
        let _ = std::io::stderr().flush();
        match extract_from_browser(&p.spec).await {
            Ok(cookie) => {
                eprintln!("found YouTube Music login.");
                write_cookie_file(cookie_file, &cookie)?;
                println!("Signed in using {} (saved to {}).", p.label, cookie_file.display());
                println!("If pova is already running, it picks this up automatically.");
                return Ok(());
            }
            Err(ExtractError::NotSignedIn { .. }) => {
                eprintln!("not signed in yet.");
                if readable_candidate.is_none() {
                    readable_candidate = Some((p.spec.clone(), p.label.clone()));
                }
                tried.push(p.label.clone());
            }
            Err(ExtractError::Other(msg)) => {
                eprintln!("skipped ({msg})");
                tried.push(p.label.clone());
            }
        }
    }

    if let Some((spec, label)) = readable_candidate {
        return wait_for_browser_login_cli(cookie_file, &spec, &label).await;
    }

    anyhow::bail!(
        "Couldn't read cookies from any detected browser profile ({}). \
         Try specifying a browser/profile with `pova login \"brave:Profile 2\"`.",
        tried.join(", ")
    );
}

async fn wait_for_browser_login_cli(cookie_file: &Path, spec: &str, label: &str) -> anyhow::Result<()> {
    if open_in_browser(spec) {
        eprintln!("Opened {YTM_SIGN_IN_URL} in {label}.");
    } else {
        eprintln!("Open {YTM_SIGN_IN_URL} in {label}.");
    }
    eprintln!("Sign in with your Google account there — waiting for login… (Ctrl+C to cancel)");
    let deadline = std::time::Instant::now() + Duration::from_secs(180);
    while std::time::Instant::now() < deadline {
        tokio::time::sleep(Duration::from_secs(3)).await;
        match extract_from_browser(spec).await {
            Ok(cookie) => {
                write_cookie_file(cookie_file, &cookie)?;
                println!("Signed in using {label} (saved to {}).", cookie_file.display());
                println!("If pova is already running, it picks this up automatically.");
                return Ok(());
            }
            Err(ExtractError::NotSignedIn { .. }) => continue,
            Err(ExtractError::Other(msg)) => anyhow::bail!("{msg}"),
        }
    }
    anyhow::bail!("Timed out waiting for sign-in in {label}. Run `pova login {spec}` again once you're signed in.");
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_netscape_jar_and_prefers_music_subdomain() {
        let jar = "\
# Netscape HTTP Cookie File
.example.com\tTRUE\t/\tTRUE\t2000000000\tSID\tignore_me
#HttpOnly_.youtube.com\tTRUE\t/\tTRUE\t2000000000\tHSID\thsid_val
.youtube.com\tTRUE\t/\tTRUE\t2000000000\tPREF\tgeneric
music.youtube.com\tFALSE\t/\tTRUE\t2000000000\tPREF\tmusic_specific
.youtube.com\tTRUE\t/\tTRUE\t2000000000\tSAPISID\tsapisid_val
";
        let cookie = parse_netscape_jar(jar, "firefox").unwrap();
        assert!(!cookie.contains("ignore_me"));
        assert!(cookie.starts_with("SAPISID=sapisid_val;"));
        assert!(cookie.contains("PREF=music_specific"));
        assert!(!cookie.contains("PREF=generic"));
        assert!(cookie.contains("HSID=hsid_val"));
    }

    #[test]
    fn jar_without_sapisid_is_rejected() {
        let jar = ".youtube.com\tTRUE\t/\tTRUE\t2000000000\tVISITOR_INFO1_LIVE\tabc\n";
        let err = parse_netscape_jar(jar, "chrome").unwrap_err().to_string();
        assert!(err.contains("No YouTube Music login found in chrome"));
    }

    #[test]
    fn single_sapisid_cookie_gets_semicolon_for_ytmapi() {
        let jar = ".youtube.com\tTRUE\t/\tTRUE\t2000000000\tSAPISID\tonly_one\n";
        let cookie = parse_netscape_jar(jar, "firefox").unwrap();
        assert_eq!(cookie, "SAPISID=only_one;");
    }

    #[test]
    fn normalizes_pasted_cookie_header() {
        assert_eq!(
            normalize_pasted_cookie("Cookie: SAPISID=abc; SID=def").unwrap(),
            "SAPISID=abc; SID=def"
        );
        assert_eq!(
            normalize_pasted_cookie("\"__Secure-3PAPISID=xyz\"").unwrap(),
            "SAPISID=xyz; __Secure-3PAPISID=xyz"
        );
        assert!(normalize_pasted_cookie("not a cookie").is_err());
        assert!(normalize_pasted_cookie("SAPISID=abc\nSID=def").is_err());
    }

    #[test]
    fn validates_browser_spec() {
        assert_eq!(
            validate_browser_spec("  chrome:Profile 2 ").unwrap(),
            "chrome:Profile 2"
        );
        assert!(validate_browser_spec("").is_err());
        assert!(validate_browser_spec("--exec=bad").is_err());
        assert!(validate_browser_spec("chrome\nfirefox").is_err());
    }

    #[test]
    fn parses_default_browser_from_mimeapps() {
        let text = "\
[Added Associations]
text/html=chromium.desktop;

[Default Applications]
x-scheme-handler/https=org.mozilla.firefox.desktop;
";
        assert_eq!(parse_default_browser_mimeapps(text), Some("firefox"));
        let chrome = "[Default Applications]\ntext/html=google-chrome.desktop\n";
        assert_eq!(parse_default_browser_mimeapps(chrome), Some("chrome"));
        let brave_origin = "[Default Applications]\nx-scheme-handler/https=brave-origin.desktop\n";
        assert_eq!(parse_default_browser_mimeapps(brave_origin), Some("brave-origin"));
        let zen = "[Default Applications]\nx-scheme-handler/https=app.zen_browser.zen.desktop\n";
        assert_eq!(parse_default_browser_mimeapps(zen), Some("zen"));
    }

    #[test]
    fn parses_chromium_local_state_profiles() {
        let json = r#"{
            "profile": {
                "last_used": "Profile 2",
                "info_cache": {
                    "Default": {"name": "Personal"},
                    "Profile 2": {"name": "Music"},
                    "Guest Profile": {"name": "Guest"}
                }
            }
        }"#;
        let mut profiles = parse_chromium_local_state(json);
        profiles.sort_by(|a, b| a.0.cmp(&b.0));
        assert_eq!(
            profiles,
            vec![
                ("Default".into(), "Personal".into(), false),
                ("Profile 2".into(), "Music".into(), true),
            ]
        );
    }

    #[test]
    fn parses_firefox_profiles_ini_entries() {
        let ini = "\
[General]
StartWithLastProfile=1

[Profile0]
Name=default-release
IsRelative=1
Path=abc123.default-release
Default=1

[Profile1]
Name=work
IsRelative=0
Path=/tmp/ff-work
";
        let profiles = parse_firefox_profiles_ini(ini);
        assert_eq!(
            profiles,
            vec![
                ("default-release".into(), "abc123.default-release".into(), true, true),
                ("work".into(), "/tmp/ff-work".into(), false, false),
            ]
        );
    }

    #[test]
    fn writes_and_removes_cookie_file_with_600_mode() {
        let dir = std::env::temp_dir().join(format!("pova-auth-write-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let path = dir.join("cookie.txt");

        write_cookie_file(&path, "SAPISID=abc; SID=def").unwrap();
        let mode = std::fs::metadata(&path).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600);
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "SAPISID=abc; SID=def\n");

        assert!(remove_cookie_file(&path).unwrap());
        assert!(!remove_cookie_file(&path).unwrap());
        let _ = std::fs::remove_dir_all(&dir);
    }
}

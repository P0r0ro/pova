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

/// Open `https://music.youtube.com` in the browser matching `spec` (falling
/// back to `xdg-open`), detached with null stdio so the TUI is never disturbed.
pub fn open_in_browser(spec: &str) -> bool {
    let browser_id = spec
        .trim()
        .split([':', '+'])
        .next()
        .unwrap_or("")
        .to_ascii_lowercase();
    let bins: &[&str] = match browser_id.as_str() {
        "firefox" => &["firefox"],
        "chrome" => &["google-chrome-stable", "google-chrome", "chrome"],
        "chromium" => &["chromium", "chromium-browser"],
        "brave" => &["brave-browser", "brave"],
        "edge" => &["microsoft-edge-stable", "microsoft-edge"],
        "vivaldi" => &["vivaldi-stable", "vivaldi"],
        "opera" => &["opera"],
        _ => &[],
    };
    for bin in bins {
        if which(bin).is_some()
            && std::process::Command::new(bin)
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
pub struct BrowserInfo {
    /// Name passed to `yt-dlp --cookies-from-browser`.
    pub id: &'static str,
    /// Human-readable label shown in the UI.
    pub label: &'static str,
    /// True if this looks like the desktop's default browser.
    pub default: bool,
}

const KNOWN_BROWSERS: [(&str, &str); 7] = [
    ("firefox", "Firefox"),
    ("chrome", "Chrome"),
    ("chromium", "Chromium"),
    ("brave", "Brave"),
    ("edge", "Edge"),
    ("vivaldi", "Vivaldi"),
    ("opera", "Opera"),
];

/// Detect browsers installed on this Linux machine (matching the directories
/// `yt-dlp --cookies-from-browser` searches), with the user's default browser
/// first. Falls back to the common browser list if none are detected.
pub fn detect_browsers() -> Vec<BrowserInfo> {
    let home = std::env::var_os("HOME").map(PathBuf::from);
    let config_home = std::env::var_os("XDG_CONFIG_HOME")
        .filter(|v| !v.is_empty())
        .map(PathBuf::from)
        .or_else(|| home.as_ref().map(|h| h.join(".config")));

    let default_id = detect_default_browser(home.as_deref(), config_home.as_deref());
    let mut found = Vec::new();

    if let (Some(home), Some(cfg)) = (home.as_deref(), config_home.as_deref()) {
        for &(id, label) in &KNOWN_BROWSERS {
            if browser_dir_exists(id, home, cfg) {
                found.push(BrowserInfo {
                    id,
                    label,
                    default: default_id == Some(id),
                });
            }
        }
    }

    if found.is_empty() {
        found = KNOWN_BROWSERS
            .iter()
            .map(|&(id, label)| BrowserInfo {
                id,
                label,
                default: default_id == Some(id),
            })
            .collect();
    }

    // Put the default browser at the top so pressing Enter works immediately.
    found.sort_by_key(|b| !b.default);
    found
}

fn browser_dir_exists(id: &str, home: &Path, config_home: &Path) -> bool {
    match id {
        "firefox" => [
            config_home.join("mozilla/firefox"),
            home.join(".mozilla/firefox"),
            home.join(".var/app/org.mozilla.firefox/config/mozilla/firefox"),
            home.join(".var/app/org.mozilla.firefox/.mozilla/firefox"),
            home.join("snap/firefox/common/.mozilla/firefox"),
        ]
        .iter()
        .any(|p| p.is_dir()),
        "chrome" => config_home.join("google-chrome").is_dir(),
        "chromium" => config_home.join("chromium").is_dir(),
        "brave" => config_home.join("BraveSoftware/Brave-Browser").is_dir(),
        "edge" => config_home.join("microsoft-edge").is_dir(),
        "vivaldi" => config_home.join("vivaldi").is_dir(),
        "opera" => config_home.join("opera").is_dir(),
        _ => false,
    }
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
    if lower.contains("firefox") {
        Some("firefox")
    } else if lower.contains("brave") {
        Some("brave")
    } else if lower.contains("vivaldi") {
        Some("vivaldi")
    } else if lower.contains("opera") {
        Some("opera")
    } else if lower.contains("edge") {
        Some("edge")
    } else if lower.contains("chromium") {
        Some("chromium")
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
    let tmp = TempJar::new().map_err(|e| ExtractError::Other(e.to_string()))?;

    // Fast path: current yt-dlp opens YoutubeDL and saves the extracted cookie
    // jar on exit without making any network request or solving JS challenges.
    let stderr = run_ytdlp_export(&spec, &tmp.jar, false)
        .await
        .map_err(|e| ExtractError::Other(e.to_string()))?;
    let jar_ready = std::fs::metadata(&tmp.jar).is_ok_and(|m| m.len() > 0);

    // Compatibility fallback for older yt-dlp builds that check URL arguments
    // before constructing YoutubeDL.
    let stderr = if !jar_ready && stderr.contains("You must provide at least one URL") && !stderr.contains("ERROR:") {
        run_ytdlp_export(&spec, &tmp.jar, true)
            .await
            .map_err(|e| ExtractError::Other(e.to_string()))?
    } else {
        stderr
    };

    let jar_text = match std::fs::read_to_string(&tmp.jar) {
        Ok(s) if !s.trim().is_empty() => s,
        _ => return Err(ExtractError::Other(explain_extract_error(&spec, &stderr))),
    };
    parse_netscape_jar(&jar_text, &spec)
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

    let browsers = detect_browsers();
    let mut readable_candidate: Option<(&str, &str)> = None;
    let mut tried = Vec::new();
    for b in &browsers {
        eprint!("Checking {}… ", b.label);
        let _ = std::io::stderr().flush();
        match extract_from_browser(b.id).await {
            Ok(cookie) => {
                eprintln!("found YouTube Music login.");
                write_cookie_file(cookie_file, &cookie)?;
                println!("Signed in using {} (saved to {}).", b.label, cookie_file.display());
                println!("If pova is already running, it picks this up automatically.");
                return Ok(());
            }
            Err(ExtractError::NotSignedIn { .. }) => {
                eprintln!("not signed in yet.");
                if readable_candidate.is_none() {
                    readable_candidate = Some((b.id, b.label));
                }
                tried.push(b.id);
            }
            Err(ExtractError::Other(msg)) => {
                eprintln!("skipped ({msg})");
                tried.push(b.id);
            }
        }
    }

    if let Some((id, label)) = readable_candidate {
        return wait_for_browser_login_cli(cookie_file, id, label).await;
    }

    anyhow::bail!(
        "Couldn't read cookies from any detected browser ({}). \
         Try specifying a browser/profile with `pova login chrome:Profile\\ 2`.",
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
        assert_eq!(validate_browser_spec("  chrome:Profile 2 ").unwrap(), "chrome:Profile 2");
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

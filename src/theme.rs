//! btop-compatible theme loading.
//!
//! pova reads btop's `.theme` files directly (`theme[key]="#rrggbb"`), so every
//! btop theme works unchanged. btop keys are mapped onto pova's UI roles; a few
//! optional pova-only keys (`viz_start`, `viz_mid`, `viz_end`, `progress_*`)
//! can override the mapping.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use ratatui::style::Color;

/// A colour that may be "unset" (use the terminal default).
pub type Rgb = Option<(u8, u8, u8)>;

#[derive(Clone, Debug)]
pub struct Theme {
    pub name: String,
    pub main_bg: Color,
    pub main_fg: Color,
    pub title: Color,
    pub hi_fg: Color,
    pub selected_bg: Color,
    pub selected_fg: Color,
    pub inactive_fg: Color,
    pub graph_text: Color,
    pub meter_bg: Color,
    pub div_line: Color,
    pub box_viz: Color,
    pub box_player: Color,
    pub box_controls: Color,
    pub accent: Color,
    viz: [Rgb; 3],
    progress: [Rgb; 3],
}

/// btop's built-in "Default" theme.
const DEFAULT_THEME: &str = r##"
theme[main_bg]="#00"
theme[main_fg]="#cc"
theme[title]="#ee"
theme[hi_fg]="#b54040"
theme[selected_bg]="#6a2f2f"
theme[selected_fg]="#ee"
theme[inactive_fg]="#40"
theme[graph_text]="#60"
theme[meter_bg]="#40"
theme[proc_misc]="#0de756"
theme[cpu_box]="#556d59"
theme[mem_box]="#6c6c4b"
theme[net_box]="#5c588d"
theme[proc_box]="#805252"
theme[div_line]="#30"
theme[cpu_start]="#77ca9b"
theme[cpu_mid]="#cbc06c"
theme[cpu_end]="#dc4c4c"
theme[download_start]="#4f43a3"
theme[download_mid]="#b0a9de"
theme[download_end]="#f0eff8"
"##;

/// Parse a btop colour value.
///
/// Accepted forms: `#rrggbb`, `#gg` (greyscale), `r g b` (decimal), or empty
/// (terminal default). Returns `None` for malformed values.
pub fn parse_color(value: &str) -> Option<Rgb> {
    let v = value.trim();
    if v.is_empty() {
        return Some(None);
    }
    if let Some(hex) = v.strip_prefix('#') {
        let byte = |s: &str| u8::from_str_radix(s, 16).ok();
        return match hex.len() {
            2 => byte(hex).map(|g| Some((g, g, g))),
            6 => Some(Some((byte(&hex[0..2])?, byte(&hex[2..4])?, byte(&hex[4..6])?))),
            _ => None,
        };
    }
    let parts: Vec<u8> = v
        .split_whitespace()
        .map(|p| p.parse::<u8>().ok())
        .collect::<Option<Vec<_>>>()?;
    match parts.as_slice() {
        [r, g, b] => Some(Some((*r, *g, *b))),
        [g] => Some(Some((*g, *g, *g))),
        _ => None,
    }
}

/// Parse the `theme[key]="value"` lines of a btop theme file.
pub fn parse_theme_file(contents: &str) -> HashMap<String, Rgb> {
    let mut map = HashMap::new();
    for line in contents.lines() {
        let line = line.trim();
        if line.starts_with('#') || !line.starts_with("theme[") {
            continue;
        }
        let Some(close) = line.find(']') else { continue };
        let key = line[6..close].trim().to_string();
        let Some(eq) = line[close..].find('=') else { continue };
        let raw = line[close + eq + 1..].trim();
        let value = if let Some(rest) = raw.strip_prefix('"') {
            rest.split_once('"').map_or(rest, |(v, _)| v)
        } else if let Some(rest) = raw.strip_prefix('\'') {
            rest.split_once('\'').map_or(rest, |(v, _)| v)
        } else {
            raw.split_once(" #").map_or(raw, |(v, _)| v).trim()
        };
        if let Some(color) = parse_color(value) {
            map.insert(key, color);
        }
    }
    map
}

fn to_color(c: Rgb) -> Color {
    match c {
        Some((r, g, b)) => Color::Rgb(r, g, b),
        None => Color::Reset,
    }
}

fn lerp(a: (u8, u8, u8), b: (u8, u8, u8), t: f32) -> (u8, u8, u8) {
    let f = |x: u8, y: u8| (x as f32 + (y as f32 - x as f32) * t).round() as u8;
    (f(a.0, b.0), f(a.1, b.1), f(a.2, b.2))
}

fn gradient(stops: &[Rgb; 3], fallback: Color, t: f32) -> Color {
    let t = t.clamp(0.0, 1.0);
    match stops {
        [Some(s), Some(m), Some(e)] => {
            let c = if t < 0.5 {
                lerp(*s, *m, t * 2.0)
            } else {
                lerp(*m, *e, (t - 0.5) * 2.0)
            };
            Color::Rgb(c.0, c.1, c.2)
        }
        [Some(s), None, Some(e)] => {
            let c = lerp(*s, *e, t);
            Color::Rgb(c.0, c.1, c.2)
        }
        [Some(s), _, _] => Color::Rgb(s.0, s.1, s.2),
        _ => fallback,
    }
}

impl Theme {
    pub fn from_map(name: &str, user: &HashMap<String, Rgb>, use_background: bool) -> Theme {
        let base = parse_theme_file(DEFAULT_THEME);
        let get = |k: &str| -> Rgb { user.get(k).copied().or_else(|| base.get(k).copied()).flatten() };
        // pova-only keys fall back to btop keys.
        let first = |keys: &[&str]| -> Rgb { keys.iter().find_map(|k| user.get(*k).copied().flatten()) };
        let viz = [
            first(&["viz_start", "cpu_start"]).or_else(|| get("cpu_start")),
            first(&["viz_mid", "cpu_mid"]).or_else(|| {
                if user.contains_key("viz_start") || user.contains_key("cpu_start") {
                    None
                } else {
                    get("cpu_mid")
                }
            }),
            first(&["viz_end", "cpu_end"]).or_else(|| get("cpu_end")),
        ];
        let progress = [
            first(&["progress_start", "download_start", "proc_misc"]).or_else(|| get("download_start")),
            first(&["progress_mid", "download_mid"]).or_else(|| {
                if user.contains_key("progress_start") || user.contains_key("download_start") {
                    None
                } else {
                    get("download_mid")
                }
            }),
            first(&["progress_end", "download_end"]).or_else(|| get("download_end")),
        ];
        let main_bg = if use_background {
            to_color(get("main_bg"))
        } else {
            Color::Reset
        };
        Theme {
            name: name.to_string(),
            main_bg,
            main_fg: to_color(get("main_fg")),
            title: to_color(get("title")),
            hi_fg: to_color(get("hi_fg")),
            selected_bg: to_color(get("selected_bg")),
            selected_fg: to_color(get("selected_fg")),
            inactive_fg: to_color(get("inactive_fg")),
            graph_text: to_color(get("graph_text")),
            meter_bg: to_color(get("meter_bg")),
            div_line: to_color(get("div_line")),
            box_viz: to_color(get("cpu_box")),
            box_player: to_color(get("proc_box")),
            box_controls: to_color(get("mem_box")),
            accent: to_color(first(&["proc_misc"]).or_else(|| get("proc_misc"))),
            viz,
            progress,
        }
    }

    pub fn builtin_default(use_background: bool) -> Theme {
        Theme::from_map("Default", &HashMap::new(), use_background)
    }

    pub fn load(path: &Path, use_background: bool) -> anyhow::Result<Theme> {
        let contents = std::fs::read_to_string(path)?;
        let name = path.file_stem().and_then(|s| s.to_str()).unwrap_or("theme");
        Ok(Theme::from_map(name, &parse_theme_file(&contents), use_background))
    }

    /// Visualizer gradient colour at height fraction `t` (0 = bottom, 1 = top).
    pub fn viz_color(&self, t: f32) -> Color {
        gradient(&self.viz, self.main_fg, t)
    }

    /// Progress-bar gradient colour at position `t` (0 = left, 1 = right).
    pub fn progress_color(&self, t: f32) -> Color {
        gradient(&self.progress, self.hi_fg, t)
    }
}

/// Directories searched for `.theme` files, highest priority first.
pub fn theme_dirs(config_dir: &Path) -> Vec<PathBuf> {
    let mut dirs = vec![config_dir.join("themes")];
    if let Some(home) = std::env::var_os("HOME") {
        dirs.push(PathBuf::from(home).join(".config/btop/themes"));
    }
    dirs.push(PathBuf::from("/usr/local/share/btop/themes"));
    dirs.push(PathBuf::from("/usr/share/btop/themes"));
    dirs
}

/// All available themes: (name, path). "Default" (built in) is always first.
pub fn list_themes(config_dir: &Path) -> Vec<(String, Option<PathBuf>)> {
    let mut seen = std::collections::HashSet::new();
    let mut found: Vec<(String, Option<PathBuf>)> = Vec::new();
    for dir in theme_dirs(config_dir) {
        let Ok(entries) = std::fs::read_dir(&dir) else { continue };
        let mut files: Vec<PathBuf> = entries
            .filter_map(|e| e.ok().map(|e| e.path()))
            .filter(|p| p.extension().is_some_and(|e| e == "theme"))
            .collect();
        files.sort();
        for path in files {
            let Some(name) = path.file_stem().and_then(|s| s.to_str()) else {
                continue;
            };
            if seen.insert(name.to_string()) {
                found.push((name.to_string(), Some(path)));
            }
        }
    }
    found.sort_by_key(|(n, _)| n.to_lowercase());
    found.insert(0, ("Default".to_string(), None));
    found
}

/// Resolve a theme by name, falling back to the built-in default.
pub fn load_named(config_dir: &Path, name: &str, use_background: bool) -> Theme {
    if !name.eq_ignore_ascii_case("default") {
        for (n, path) in list_themes(config_dir) {
            if n.eq_ignore_ascii_case(name)
                && let Some(path) = path
            {
                match Theme::load(&path, use_background) {
                    Ok(t) => return t,
                    Err(e) => tracing::warn!("failed to load theme {}: {e}", path.display()),
                }
            }
        }
        tracing::warn!("theme {name:?} not found, using Default");
    }
    Theme::builtin_default(use_background)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_color_forms() {
        assert_eq!(parse_color("#ff8000"), Some(Some((255, 128, 0))));
        assert_eq!(parse_color("#40"), Some(Some((64, 64, 64))));
        assert_eq!(parse_color("10 20 30"), Some(Some((10, 20, 30))));
        assert_eq!(parse_color(""), Some(None));
        assert_eq!(parse_color("#12345"), None);
        assert_eq!(parse_color("nope"), None);
    }

    #[test]
    fn parses_theme_lines() {
        let m = parse_theme_file(
            "# c\ntheme[main_bg]=\"#282a36\" # inline comment\ntheme[ title ] = \"#f8f8f2\"\njunk\n",
        );
        assert_eq!(m.get("main_bg"), Some(&Some((0x28, 0x2a, 0x36))));
        assert_eq!(m.get("title"), Some(&Some((0xf8, 0xf8, 0xf2))));
    }

    #[test]
    fn custom_two_stop_pova_gradient_ignores_default_mid() {
        let m = parse_theme_file("theme[viz_start]=\"#000000\"\ntheme[viz_end]=\"#ffffff\"\n");
        let t = Theme::from_map("custom", &m, true);
        assert_eq!(t.viz_color(0.5), Color::Rgb(0x80, 0x80, 0x80));
    }

    #[test]
    fn gradient_endpoints() {
        let t = Theme::builtin_default(true);
        assert_eq!(t.viz_color(0.0), Color::Rgb(0x77, 0xca, 0x9b));
        assert_eq!(t.viz_color(0.5), Color::Rgb(0xcb, 0xc0, 0x6c));
        assert_eq!(t.viz_color(1.0), Color::Rgb(0xdc, 0x4c, 0x4c));
    }

    /// Every theme shipped with btop must parse and map to concrete colours.
    #[test]
    fn all_btop_themes_parse() {
        let dirs = [
            Path::new(env!("CARGO_MANIFEST_DIR")).join("../btop/themes"),
            PathBuf::from("/usr/share/btop/themes"),
        ];
        let mut count = 0;
        for dir in dirs.iter().filter(|d| d.is_dir()) {
            for entry in std::fs::read_dir(dir).unwrap() {
                let path = entry.unwrap().path();
                if path.extension().is_none_or(|e| e != "theme") {
                    continue;
                }
                let map = parse_theme_file(&std::fs::read_to_string(&path).unwrap());
                assert!(map.contains_key("main_fg"), "{} has no main_fg", path.display());
                let theme = Theme::load(&path, true).unwrap();
                assert_ne!(theme.main_fg, Color::Reset, "{}", path.display());
                assert!(matches!(theme.viz_color(0.3), Color::Rgb(..)), "{}", path.display());
                count += 1;
            }
        }
        if dirs.iter().any(|d| d.is_dir()) {
            assert!(count > 10, "only {count} themes found");
        }
    }
}

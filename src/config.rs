//! User configuration (`~/.config/pova/config.toml`) and persisted UI state
//! (`~/.local/state/pova/state.toml`, so we never clobber the user's comments).

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use anyhow::Context;
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug)]
pub struct Paths {
    pub config_dir: PathBuf,
    pub cache_dir: PathBuf,
    pub state_dir: PathBuf,
    pub runtime_dir: PathBuf,
}

impl Paths {
    pub fn new() -> anyhow::Result<Paths> {
        let dirs = directories::ProjectDirs::from("", "", "pova").context("no home directory")?;
        let state_dir = dirs.state_dir().unwrap_or_else(|| dirs.data_local_dir()).to_path_buf();
        let runtime_dir = dirs
            .runtime_dir()
            .map(Path::to_path_buf)
            .unwrap_or_else(std::env::temp_dir);
        let p = Paths {
            config_dir: dirs.config_dir().to_path_buf(),
            cache_dir: dirs.cache_dir().to_path_buf(),
            state_dir,
            runtime_dir,
        };
        for d in [&p.config_dir, &p.cache_dir, &p.state_dir, &p.runtime_dir] {
            std::fs::create_dir_all(d).with_context(|| format!("creating {}", d.display()))?;
        }
        std::fs::create_dir_all(p.cache_dir.join("art"))?;
        Ok(p)
    }

    pub fn config_file(&self) -> PathBuf {
        self.config_dir.join("config.toml")
    }
    pub fn cookie_file(&self) -> PathBuf {
        self.config_dir.join("cookie.txt")
    }
    pub fn state_file(&self) -> PathBuf {
        self.state_dir.join("state.toml")
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum VizMode {
    #[default]
    Bars,
    Mirror,
    Mono,
    Wave,
}

impl VizMode {
    pub fn next(self) -> VizMode {
        match self {
            VizMode::Bars => VizMode::Mirror,
            VizMode::Mirror => VizMode::Mono,
            VizMode::Mono => VizMode::Wave,
            VizMode::Wave => VizMode::Bars,
        }
    }
    pub fn label(self) -> &'static str {
        match self {
            VizMode::Bars => "bars",
            VizMode::Mirror => "mirror",
            VizMode::Mono => "mono",
            VizMode::Wave => "wave",
        }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum VizSource {
    /// Only mpv's own PipeWire stream (other apps are ignored).
    #[default]
    Mpv,
    /// Whatever plays on the default output (like plain cava).
    System,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default)]
pub struct VizConfig {
    pub mode: VizMode,
    pub source: VizSource,
    pub bar_width: u16,
    pub bar_spacing: u16,
    /// 0..1, cava's `noise_reduction`: higher = smoother/slower.
    pub noise_reduction: f64,
    pub framerate: u32,
    pub low_cut_off: u32,
    pub high_cut_off: u32,
    pub sample_rate: u32,
}

impl Default for VizConfig {
    fn default() -> Self {
        VizConfig {
            mode: VizMode::Bars,
            source: VizSource::Mpv,
            bar_width: 2,
            bar_spacing: 1,
            noise_reduction: 0.77,
            framerate: 60,
            low_cut_off: 50,
            high_cut_off: 10000,
            sample_rate: 44100,
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default)]
pub struct Config {
    /// Theme name (file stem of a btop `.theme` file) or "Default".
    pub theme: String,
    /// Paint the theme's `main_bg`. false = keep the terminal background (transparency).
    pub theme_background: bool,
    pub rounded_corners: bool,
    /// Initial volume, 0-100.
    pub volume: u8,
    /// Append a radio of similar songs when the queue runs out.
    pub autoplay: bool,
    /// Render album art (kitty graphics protocol / halfblocks fallback).
    pub album_art: bool,
    /// "auto" (query capable terminals, else halfblocks), "kitty", "sixel",
    /// "iterm2" or "halfblocks".
    pub image_protocol: String,
    /// Extra arguments passed to mpv.
    pub mpv_args: Vec<String>,
    /// Action name -> key(s), e.g. `next = ["N", "ctrl+right"]`.
    pub keys: HashMap<String, Vec<String>>,
    pub viz: VizConfig,
}

impl Default for Config {
    fn default() -> Self {
        Config {
            theme: "Default".into(),
            theme_background: true,
            rounded_corners: true,
            volume: 70,
            autoplay: true,
            album_art: true,
            image_protocol: "auto".into(),
            mpv_args: Vec::new(),
            keys: HashMap::new(),
            viz: VizConfig::default(),
        }
    }
}

impl Config {
    pub fn load(path: &Path) -> anyhow::Result<Config> {
        if !path.exists() {
            let cfg = Config::default();
            // Write a starter file so users can discover the options.
            if let Ok(text) = toml::to_string_pretty(&cfg) {
                let _ = std::fs::write(path, format!("# pova configuration\n\n{text}"));
            }
            return Ok(cfg);
        }
        let text = std::fs::read_to_string(path)?;
        toml::from_str(&text).with_context(|| format!("parsing {}", path.display()))
    }
}

/// UI state remembered between runs.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct State {
    pub theme: Option<String>,
    pub volume: Option<u8>,
    pub viz_mode: Option<VizMode>,
    pub show_viz: Option<bool>,
    pub show_player: Option<bool>,
    pub show_controls: Option<bool>,
    pub autoplay: Option<bool>,
    pub album_art: Option<bool>,
}

impl State {
    pub fn load(path: &Path) -> State {
        std::fs::read_to_string(path)
            .ok()
            .and_then(|t| toml::from_str(&t).ok())
            .unwrap_or_default()
    }
    pub fn save(&self, path: &Path) {
        if let Ok(text) = toml::to_string_pretty(self)
            && let Err(e) = std::fs::write(path, text)
        {
            tracing::warn!("saving state: {e}");
        }
    }
}

use std::fs;
use std::path::PathBuf;

use gtk::glib;
use serde::{Deserialize, Serialize};

pub const CHANNEL_COUNT: usize = 8;

const DEFAULT_NAMES: [&str; CHANNEL_COUNT] = [
    "Microphone",
    "Game",
    "Music",
    "Voice Chat",
    "Browser",
    "System",
    "SFX",
    "Aux",
];

/// What feeds an input channel: either a capture source (hardware input or a
/// monitor of another device), or an application's playback stream matched by
/// its `application.name`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Assignment {
    Source { name: String },
    App { name: String },
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default)]
pub struct ChannelConfig {
    pub name: String,
    pub assignment: Option<Assignment>,
    pub monitor_volume: f64,
    pub stream_volume: f64,
    pub monitor_muted: bool,
    pub stream_muted: bool,
    pub linked: bool,
}

impl Default for ChannelConfig {
    fn default() -> Self {
        Self {
            name: String::new(),
            assignment: None,
            monitor_volume: 0.75,
            stream_volume: 0.75,
            monitor_muted: false,
            stream_muted: false,
            linked: false,
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default)]
pub struct MasterConfig {
    /// Hardware sink the monitor mix is routed to; `None` = system default.
    pub monitor_device: Option<String>,
    pub monitor_volume: f64,
    pub stream_volume: f64,
    pub monitor_muted: bool,
    pub stream_muted: bool,
}

impl Default for MasterConfig {
    fn default() -> Self {
        Self {
            monitor_device: None,
            monitor_volume: 1.0,
            stream_volume: 1.0,
            monitor_muted: false,
            stream_muted: false,
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default)]
pub struct Config {
    pub channels: Vec<ChannelConfig>,
    pub master: MasterConfig,
}

impl Default for Config {
    fn default() -> Self {
        let channels = DEFAULT_NAMES
            .iter()
            .map(|n| ChannelConfig {
                name: (*n).to_string(),
                ..ChannelConfig::default()
            })
            .collect();
        Self {
            channels,
            master: MasterConfig::default(),
        }
    }
}

impl Config {
    fn path() -> PathBuf {
        glib::user_config_dir().join("openwave").join("config.json")
    }

    pub fn load() -> Self {
        let mut cfg: Config = fs::read_to_string(Self::path())
            .ok()
            .and_then(|s| serde_json::from_str(&s).ok())
            .unwrap_or_default();
        cfg.channels.truncate(CHANNEL_COUNT);
        cfg.channels
            .resize_with(CHANNEL_COUNT, ChannelConfig::default);
        for (i, ch) in cfg.channels.iter_mut().enumerate() {
            if ch.name.trim().is_empty() {
                ch.name = DEFAULT_NAMES[i].to_string();
            }
            ch.monitor_volume = ch.monitor_volume.clamp(0.0, 1.0);
            ch.stream_volume = ch.stream_volume.clamp(0.0, 1.0);
        }
        cfg.master.monitor_volume = cfg.master.monitor_volume.clamp(0.0, 1.0);
        cfg.master.stream_volume = cfg.master.stream_volume.clamp(0.0, 1.0);
        cfg
    }

    pub fn save(&self) {
        let path = Self::path();
        if let Some(dir) = path.parent() {
            let _ = fs::create_dir_all(dir);
        }
        if let Ok(s) = serde_json::to_string_pretty(self) {
            let _ = fs::write(path, s);
        }
    }
}

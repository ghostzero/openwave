use std::collections::{BTreeMap, HashSet};
use std::fs;
use std::path::PathBuf;

use gtk::glib;
use serde::{Deserialize, Serialize};

pub const MAX_CHANNELS: usize = 8;

/// Default names offered when adding channels via "+".
pub const TEMPLATE_NAMES: [&str; 6] = ["Game", "Music", "Voice Chat", "Browser", "SFX", "Aux"];

/// What feeds an input channel: a capture source (hardware input or a monitor
/// of another device), an application's playback stream matched by its
/// `application.name`, or a standalone virtual device ("OpenWave: <name>")
/// that other software can select as an output.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Assignment {
    Source { name: String },
    App { name: String },
    Virtual,
}

/// One LV2 plugin instance in a channel's effect chain.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct EffectConfig {
    /// Stable identifier, unique within the channel; used as the node label
    /// in the generated filter-chain graph, so it must survive reordering.
    pub id: u64,
    /// LV2 plugin URI.
    pub uri: String,
    /// Display name (cached from the plugin so the UI works even when the
    /// plugin is uninstalled later).
    pub name: String,
    pub enabled: bool,
    /// Control-port values by port symbol; ports not listed here keep the
    /// plugin's default.
    pub controls: BTreeMap<String, f64>,
}

impl Default for EffectConfig {
    fn default() -> Self {
        Self {
            id: 0,
            uri: String::new(),
            name: String::new(),
            enabled: true,
            controls: BTreeMap::new(),
        }
    }
}

/// One VST2/VST3 plugin in a channel's rack, hosted by the helper process.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct VstPluginConfig {
    /// Stable identifier, unique within the channel.
    pub id: u64,
    /// Path to the .so / .vst3 binary.
    pub path: String,
    pub format: crate::vst::VstFormat,
    /// Sub-plugin label inside a multi-plugin bundle (VST3); empty otherwise.
    pub label: String,
    /// Sub-plugin selector for multi-plugin binaries (0 = whole file).
    pub unique_id: i64,
    /// Display name (cached from discovery).
    pub name: String,
    pub enabled: bool,
    /// Parameter values by parameter index (as decimal string keys, since
    /// JSON object keys are strings).
    pub params: BTreeMap<String, f64>,
}

impl Default for VstPluginConfig {
    fn default() -> Self {
        Self {
            id: 0,
            path: String::new(),
            format: crate::vst::VstFormat::Vst2,
            label: String::new(),
            unique_id: 0,
            name: String::new(),
            enabled: true,
            params: BTreeMap::new(),
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default)]
pub struct ChannelConfig {
    /// Stable identifier; never reused, survives reordering and removal.
    pub id: u64,
    pub name: String,
    pub assignment: Option<Assignment>,
    pub monitor_volume: f64,
    pub stream_volume: f64,
    pub monitor_muted: bool,
    pub stream_muted: bool,
    pub linked: bool,
    /// Built-in channels (Microphone, System) that cannot be removed.
    pub permanent: bool,
    /// LV2 effect chain applied to this channel's input before it reaches
    /// the monitor/stream mixes.
    pub effects: Vec<EffectConfig>,
    /// Next effect/VST id for this channel; never reused, shared by both
    /// lists.
    pub next_effect_id: u64,
    /// VST rack processed in front of the LV2 chain.
    pub vst_plugins: Vec<VstPluginConfig>,
}

impl Default for ChannelConfig {
    fn default() -> Self {
        Self {
            id: 0,
            name: String::new(),
            assignment: None,
            monitor_volume: 0.75,
            stream_volume: 0.75,
            monitor_muted: false,
            stream_muted: false,
            linked: false,
            permanent: false,
            effects: Vec::new(),
            next_effect_id: 1,
            vst_plugins: Vec::new(),
        }
    }
}

impl ChannelConfig {
    /// Effects that should actually be instantiated in the chain.
    pub fn enabled_effects(&self) -> Vec<&EffectConfig> {
        self.effects.iter().filter(|e| e.enabled).collect()
    }

    /// VST plugins that should actually be loaded into the rack.
    pub fn enabled_vsts(&self) -> Vec<&VstPluginConfig> {
        self.vst_plugins.iter().filter(|p| p.enabled).collect()
    }

    /// Whether this channel routes through an FX bridge at all.
    pub fn fx_active(&self) -> bool {
        self.effects.iter().any(|e| e.enabled)
            || self.vst_plugins.iter().any(|p| p.enabled)
    }

    /// Append an effect and return a reference to it.
    pub fn add_effect(&mut self, uri: &str, name: &str) -> &EffectConfig {
        let id = self.next_effect_id;
        self.next_effect_id += 1;
        self.effects.push(EffectConfig {
            id,
            uri: uri.to_string(),
            name: name.to_string(),
            ..EffectConfig::default()
        });
        self.effects.last().unwrap()
    }

    pub fn effect_mut(&mut self, effect_id: u64) -> Option<&mut EffectConfig> {
        self.effects.iter_mut().find(|e| e.id == effect_id)
    }

    /// Insert an effect at the front of the chain (before everything else)
    /// and return a reference to it.
    pub fn insert_effect_front(&mut self, uri: &str, name: &str) -> &EffectConfig {
        let id = self.next_effect_id;
        self.next_effect_id += 1;
        self.effects.insert(
            0,
            EffectConfig {
                id,
                uri: uri.to_string(),
                name: name.to_string(),
                ..EffectConfig::default()
            },
        );
        &self.effects[0]
    }

    /// Whether the one-click noise suppression is on, i.e. an enabled RNNoise
    /// effect sits in the chain.
    pub fn noise_suppression_active(&self) -> bool {
        self.effects
            .iter()
            .any(|e| e.enabled && crate::lv2::is_rnnoise(&e.uri))
    }

    /// Append a VST plugin from a discovery entry.
    pub fn add_vst(&mut self, entry: &crate::vst::VstEntry) -> u64 {
        let id = self.next_effect_id;
        self.next_effect_id += 1;
        self.vst_plugins.push(VstPluginConfig {
            id,
            path: entry.path.clone(),
            format: entry.format,
            label: entry.label.clone(),
            unique_id: entry.unique_id,
            name: entry.name.clone(),
            ..VstPluginConfig::default()
        });
        id
    }

    pub fn vst_mut(&mut self, vst_id: u64) -> Option<&mut VstPluginConfig> {
        self.vst_plugins.iter_mut().find(|p| p.id == vst_id)
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

/// Volume levels re-applied to an Elgato Wave XLR every time OpenWave starts
/// (the device occasionally forgets its volume settings). Percentages 0–100;
/// `None` = leave that control alone.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct WaveXlrConfig {
    pub mic_volume: Option<f64>,
    pub output_volume: Option<f64>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default)]
pub struct Config {
    pub next_channel_id: u64,
    pub channels: Vec<ChannelConfig>,
    pub master: MasterConfig,
    /// The setup assistant was shown once; afterwards misconfigurations only
    /// produce a notice instead of the full dialog.
    pub setup_done: bool,
    pub wave_xlr: WaveXlrConfig,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            next_channel_id: 3,
            channels: vec![
                ChannelConfig {
                    id: 1,
                    name: "Microphone".to_string(),
                    permanent: true,
                    ..ChannelConfig::default()
                },
                ChannelConfig {
                    id: 2,
                    name: "System".to_string(),
                    assignment: Some(Assignment::Virtual),
                    permanent: true,
                    ..ChannelConfig::default()
                },
            ],
            master: MasterConfig::default(),
            setup_done: false,
            wave_xlr: WaveXlrConfig::default(),
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

        // Migration from the fixed-8-channel format (no ids): drop the
        // untouched filler channels, keep anything assigned plus the two
        // defaults.
        if cfg.channels.iter().any(|c| c.id == 0) {
            cfg.channels.retain(|c| {
                c.assignment.is_some() || c.name == "Microphone" || c.name == "System"
            });
        }

        let mut seen: HashSet<u64> = HashSet::new();
        let mut max_id = 0;
        for ch in &mut cfg.channels {
            if ch.id == 0 || seen.contains(&ch.id) {
                ch.id = cfg.next_channel_id.max(max_id + 1);
            }
            seen.insert(ch.id);
            max_id = max_id.max(ch.id);
            if ch.name.trim().is_empty() {
                ch.name = format!("Channel {}", ch.id);
            }
            // Idempotent: also upgrades configs saved before the flag existed.
            if ch.name == "Microphone" || ch.name == "System" {
                ch.permanent = true;
            }
            ch.monitor_volume = ch.monitor_volume.clamp(0.0, 1.0);
            ch.stream_volume = ch.stream_volume.clamp(0.0, 1.0);
            // Repair effect/VST ids the same way as channel ids (they share
            // one id space per channel).
            let mut seen_fx: HashSet<u64> = HashSet::new();
            let mut max_fx = 0;
            for fx in &mut ch.effects {
                if fx.id == 0 || seen_fx.contains(&fx.id) {
                    fx.id = ch.next_effect_id.max(max_fx + 1);
                }
                seen_fx.insert(fx.id);
                max_fx = max_fx.max(fx.id);
            }
            for p in &mut ch.vst_plugins {
                if p.id == 0 || seen_fx.contains(&p.id) {
                    p.id = ch.next_effect_id.max(max_fx + 1);
                }
                seen_fx.insert(p.id);
                max_fx = max_fx.max(p.id);
            }
            ch.effects.retain(|fx| !fx.uri.is_empty());
            ch.vst_plugins.retain(|p| !p.path.is_empty());
            ch.next_effect_id = ch.next_effect_id.max(max_fx + 1);
        }
        cfg.channels.truncate(MAX_CHANNELS);
        cfg.next_channel_id = cfg.next_channel_id.max(max_id + 1);
        cfg.master.monitor_volume = cfg.master.monitor_volume.clamp(0.0, 1.0);
        cfg.master.stream_volume = cfg.master.stream_volume.clamp(0.0, 1.0);
        cfg.wave_xlr.mic_volume = cfg.wave_xlr.mic_volume.map(|v| v.clamp(0.0, 100.0));
        cfg.wave_xlr.output_volume = cfg.wave_xlr.output_volume.map(|v| v.clamp(0.0, 100.0));
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

    pub fn channel(&self, id: u64) -> Option<&ChannelConfig> {
        self.channels.iter().find(|c| c.id == id)
    }

    pub fn channel_mut(&mut self, id: u64) -> Option<&mut ChannelConfig> {
        self.channels.iter_mut().find(|c| c.id == id)
    }

    /// Template names not yet used by an existing channel.
    pub fn unused_template_names(&self) -> Vec<&'static str> {
        TEMPLATE_NAMES
            .iter()
            .copied()
            .filter(|t| !self.channels.iter().any(|c| c.name == *t))
            .collect()
    }

    /// Add a channel (as a virtual device by default). Returns its id, or
    /// `None` when the channel limit is reached.
    pub fn add_channel(&mut self, name: Option<&str>) -> Option<u64> {
        if self.channels.len() >= MAX_CHANNELS {
            return None;
        }
        let id = self.next_channel_id;
        self.next_channel_id += 1;
        let name = name
            .map(str::to_string)
            .or_else(|| self.unused_template_names().first().map(|s| s.to_string()))
            .unwrap_or_else(|| format!("Channel {id}"));
        self.channels.push(ChannelConfig {
            id,
            name,
            assignment: Some(Assignment::Virtual),
            ..ChannelConfig::default()
        });
        Some(id)
    }

    pub fn remove_channel(&mut self, id: u64) {
        self.channels.retain(|c| c.id != id || c.permanent);
    }
}

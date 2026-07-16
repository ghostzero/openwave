//! Per-channel effect processing outside the PulseAudio API.
//!
//! Two kinds of helper processes are managed here, both children of the app
//! (and tied to its lifetime via PR_SET_PDEATHSIG):
//!
//! * The LV2 chain: a `pipewire -c <generated conf>` process hosting
//!   `libpipewire-module-filter-chain`. It exposes a sink
//!   (`OpenWave_Ch<id>_FX`) and a source (`OpenWave_Ch<id>_FXOut`) that the
//!   regular loopback plumbing in `audio.rs` routes through. Control-port
//!   changes are applied live with `pw-cli set-param <node> Props …`.
//!
//! * The optional Carla rack (`carla-rack`), a VST2/VST3 host. It shows up
//!   as a JACK client named `OpenWave_Ch<id>_VST` (via PIPEWIRE_PROPS); it
//!   is wired in front of the chain sink with `pw-link` against a dedicated
//!   null sink's monitor, because JACK clients are invisible to the
//!   PulseAudio API.

use std::cell::Cell;
use std::collections::HashMap;
use std::fs;
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::rc::Rc;
use std::time::Instant;

use gtk::{gio, glib};

use crate::config::ChannelConfig;
use crate::lv2::Catalog;

pub fn chain_sink_name(id: u64) -> String {
    format!("OpenWave_Ch{id}_FX")
}

pub fn chain_source_name(id: u64) -> String {
    format!("OpenWave_Ch{id}_FXOut")
}

pub fn vstin_sink_name(id: u64) -> String {
    format!("OpenWave_Ch{id}_VSTIn")
}

pub fn carla_node_name(id: u64) -> String {
    format!("OpenWave_Ch{id}_VST")
}

/// Node labels inside the filter graph for one effect ("fx<id>", or one per
/// stereo lane when the plugin is mono and instantiated twice).
pub fn effect_labels(effect_id: u64, mono: bool) -> Vec<String> {
    if mono {
        vec![format!("fx{effect_id}_l"), format!("fx{effect_id}_r")]
    } else {
        vec![format!("fx{effect_id}")]
    }
}

#[derive(Clone, Copy, Debug)]
pub enum FxEvent {
    /// The chain process for a channel exited without being asked to.
    ChainDied(u64),
    /// The Carla rack for a channel exited (user closed the window, crash)
    /// or never became linkable.
    CarlaDied(u64),
}

struct Slot {
    pid: i32,
    alive: Rc<Cell<bool>>,
    expect_exit: Rc<Cell<bool>>,
    /// Set when the process died shortly after spawning (bad plugin, bad
    /// conf) so callers stop waiting for its nodes.
    failed: Rc<Cell<bool>>,
}

impl Slot {
    fn running(&self) -> bool {
        self.alive.get() && !self.failed.get()
    }

    fn kill(&self) {
        if self.alive.get() {
            self.expect_exit.set(true);
            unsafe {
                // Negative pid: the whole process group (Carla forks).
                libc::kill(-self.pid, libc::SIGTERM);
            }
        }
    }
}

struct ChainSlot {
    slot: Slot,
    conf: String,
    /// Consecutive quick deaths with this exact conf; at 2 we stop
    /// respawning so a plugin that crashes on load can't cause a loop.
    fails: u32,
}

struct CarlaSlot {
    slot: Slot,
    gui: bool,
    /// Cancellation flag for the link-poll timeout. The poll source removes
    /// itself (returns Break) when this is set — never remove a stored
    /// SourceId here, a source that already broke would panic on remove.
    link_cancel: Option<Rc<Cell<bool>>>,
}

impl Drop for CarlaSlot {
    fn drop(&mut self) {
        if let Some(c) = self.link_cancel.take() {
            c.set(true);
        }
    }
}

#[derive(Default)]
pub struct FxManager {
    chains: HashMap<u64, ChainSlot>,
    carlas: HashMap<u64, CarlaSlot>,
    handler: Option<Rc<dyn Fn(FxEvent)>>,
}

fn fx_dir() -> PathBuf {
    glib::user_config_dir().join("openwave").join("fx")
}

fn carla_dir() -> PathBuf {
    glib::user_config_dir().join("openwave").join("carla")
}

fn sanitize_token(s: &str) -> String {
    s.chars()
        .filter(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '-' | '.'))
        .collect()
}

fn sanitize_uri(s: &str) -> String {
    s.chars()
        .filter(|c| !c.is_whitespace() && !matches!(c, '"' | '\'' | '\\'))
        .collect()
}

/// Spawn a child in its own process group that dies with us.
fn spawn_child(argv: &[String], envs: &[(String, String)], log: &str) -> Option<i32> {
    use std::os::unix::process::CommandExt;
    let mut cmd = Command::new(&argv[0]);
    cmd.args(&argv[1..]);
    for (k, v) in envs {
        cmd.env(k, v);
    }
    let out = fs::File::create(glib::user_runtime_dir().join(log)).ok();
    cmd.stdin(Stdio::null());
    match out {
        Some(f) => {
            if let Ok(f2) = f.try_clone() {
                cmd.stdout(Stdio::from(f));
                cmd.stderr(Stdio::from(f2));
            } else {
                cmd.stdout(Stdio::from(f));
                cmd.stderr(Stdio::null());
            }
        }
        None => {
            cmd.stdout(Stdio::null());
            cmd.stderr(Stdio::null());
        }
    }
    cmd.process_group(0);
    unsafe {
        cmd.pre_exec(|| {
            libc::prctl(libc::PR_SET_PDEATHSIG, libc::SIGTERM);
            Ok(())
        });
    }
    cmd.spawn().ok().map(|c| c.id() as i32)
}

impl FxManager {
    pub fn set_event_handler(&mut self, f: impl Fn(FxEvent) + 'static) {
        self.handler = Some(Rc::new(f));
    }

    fn watch(&self, pid: i32, slot: &Slot, event: FxEvent, min_uptime_failed: bool) {
        let alive = slot.alive.clone();
        let expect = slot.expect_exit.clone();
        let failed = slot.failed.clone();
        let handler = self.handler.clone();
        let started = Instant::now();
        glib::child_watch_add_local(glib::Pid(pid), move |_, _| {
            alive.set(false);
            if expect.get() {
                return;
            }
            if min_uptime_failed && started.elapsed().as_secs() < 5 {
                failed.set(true);
            }
            if let Some(h) = &handler {
                h(event);
            }
        });
    }

    // ---- LV2 chain ---------------------------------------------------------

    /// Make sure the chain process for this channel runs `conf` (respawning
    /// on content changes), or kill it when `conf` is None.
    pub fn ensure_chain(&mut self, id: u64, conf: Option<String>) {
        let Some(conf) = conf else {
            if let Some(c) = self.chains.remove(&id) {
                c.slot.kill();
            }
            return;
        };
        let mut fails = 0;
        if let Some(c) = self.chains.get(&id) {
            if c.conf == conf {
                if c.slot.running() {
                    return;
                }
                if c.slot.failed.get() {
                    fails = c.fails + 1;
                    if fails >= 2 {
                        return; // known-bad conf; keep the dead slot as a marker
                    }
                }
            }
        }
        if let Some(c) = self.chains.remove(&id) {
            c.slot.kill();
        }
        let dir = fx_dir();
        let _ = fs::create_dir_all(&dir);
        let path = dir.join(format!("ch{id}.conf"));
        if fs::write(&path, &conf).is_err() {
            return;
        }
        let slot = Slot {
            pid: 0,
            alive: Rc::new(Cell::new(true)),
            expect_exit: Rc::new(Cell::new(false)),
            failed: Rc::new(Cell::new(false)),
        };
        let Some(pid) = spawn_child(
            &[
                "pipewire".to_string(),
                "-c".to_string(),
                path.display().to_string(),
            ],
            &[],
            &format!("openwave-fx-ch{id}.log"),
        ) else {
            return;
        };
        let slot = Slot { pid, ..slot };
        self.watch(pid, &slot, FxEvent::ChainDied(id), true);
        self.chains.insert(id, ChainSlot { slot, conf, fails });
    }

    pub fn chain_running(&self, id: u64) -> bool {
        self.chains.get(&id).is_some_and(|c| c.slot.running())
    }

    /// The chain died right after spawning; callers waiting for its nodes
    /// should wire the channel without it.
    pub fn chain_failed(&self, id: u64) -> bool {
        self.chains.get(&id).is_none_or(|c| !c.slot.running())
    }

    /// Apply a control-port value to the live chain of a channel.
    pub fn set_controls(&self, channel: u64, labels: &[String], symbol: &str, value: f64) {
        if !self.chain_running(channel) {
            return;
        }
        let node = chain_sink_name(channel);
        let symbol = sanitize_token(symbol);
        let entries: String = labels
            .iter()
            .map(|l| format!("\"{}:{}\" {:.6} ", sanitize_token(l), symbol, value))
            .collect();
        let cmd = format!(
            "pw-cli set-param {node} Props '{{ params = [ {entries}] }}'"
        );
        let _ = glib::spawn_command_line_async(cmd);
    }

    // ---- Carla rack --------------------------------------------------------

    pub fn carla_available() -> bool {
        glib::find_program_in_path("carla-rack").is_some()
    }

    pub fn carla_running(&self, id: u64) -> bool {
        self.carlas.get(&id).is_some_and(|c| c.slot.running())
    }

    fn carla_project(id: u64) -> Option<PathBuf> {
        let dir = carla_dir();
        fs::create_dir_all(&dir).ok()?;
        let path = dir.join(format!("ch{id}.carxp"));
        if !path.exists() {
            fs::write(
                &path,
                "<?xml version='1.0' encoding='UTF-8'?>\n\
                 <!DOCTYPE CARLA-PROJECT>\n\
                 <CARLA-PROJECT VERSION='2.5'>\n\
                 </CARLA-PROJECT>\n",
            )
            .ok()?;
        }
        Some(path)
    }

    fn spawn_carla(&mut self, id: u64, gui: bool) -> bool {
        let Some(bin) = glib::find_program_in_path("carla-rack") else {
            return false;
        };
        let Some(project) = Self::carla_project(id) else {
            return false;
        };
        let mut argv = vec![bin.display().to_string()];
        if !gui {
            argv.push("--no-gui".to_string());
        }
        argv.push(project.display().to_string());
        let envs = [(
            "PIPEWIRE_PROPS".to_string(),
            format!("{{ node.name = \"{}\" }}", carla_node_name(id)),
        )];
        let slot = Slot {
            pid: 0,
            alive: Rc::new(Cell::new(true)),
            expect_exit: Rc::new(Cell::new(false)),
            failed: Rc::new(Cell::new(false)),
        };
        let Some(pid) = spawn_child(&argv, &envs, &format!("openwave-carla-ch{id}.log"))
        else {
            return false;
        };
        let slot = Slot { pid, ..slot };
        self.watch(pid, &slot, FxEvent::CarlaDied(id), false);
        self.carlas.insert(
            id,
            CarlaSlot {
                slot,
                gui,
                link_cancel: None,
            },
        );
        true
    }

    /// Ensure a (headless) rack process exists; returns whether one runs.
    pub fn ensure_carla(&mut self, id: u64) -> bool {
        if self.carla_running(id) {
            return true;
        }
        self.carlas.remove(&id).map(|c| c.slot.kill());
        self.spawn_carla(id, false)
    }

    /// Bring up the rack with its editor window. A headless instance is
    /// restarted with the GUI (it reloads the saved project). Returns true
    /// when the rack was already running before the call.
    pub fn open_carla_gui(&mut self, id: u64) -> bool {
        if let Some(c) = self.carlas.get(&id) {
            if c.slot.running() && c.gui {
                return true;
            }
        }
        let was_running = self.carla_running(id);
        if let Some(c) = self.carlas.remove(&id) {
            c.slot.kill();
        }
        self.spawn_carla(id, true);
        was_running
    }

    pub fn kill_carla(&mut self, id: u64) {
        if let Some(c) = self.carlas.remove(&id) {
            c.slot.kill();
        }
    }

    /// Keep trying to wire VSTIn.monitor → Carla → chain sink until the
    /// Carla node's ports exist. `pw-link` exits with "File exists" once a
    /// link is up, which counts as success; "No such object" means the node
    /// is not there yet.
    pub fn start_carla_links(&mut self, id: u64) {
        let Some(c) = self.carlas.get_mut(&id) else {
            return;
        };
        if let Some(old) = c.link_cancel.take() {
            old.set(true);
        }
        let cancel = Rc::new(Cell::new(false));
        c.link_cancel = Some(cancel.clone());
        let vstin = vstin_sink_name(id);
        let carla = carla_node_name(id);
        let sink = chain_sink_name(id);
        let script = format!(
            "pw-link '{vstin}:monitor_FL' '{carla}:audio-in1' 2>&1; \
             pw-link '{vstin}:monitor_FR' '{carla}:audio-in2' 2>&1; \
             pw-link '{carla}:audio-out1' '{sink}:playback_FL' 2>&1; \
             pw-link '{carla}:audio-out2' '{sink}:playback_FR' 2>&1; \
             exit 0"
        );
        let alive = c.slot.alive.clone();
        let handler = self.handler.clone();
        let tries = Rc::new(Cell::new(0u32));
        let done = Rc::new(Cell::new(false));
        glib::timeout_add_local(std::time::Duration::from_millis(800), move || {
            if cancel.get() || done.get() || !alive.get() {
                return glib::ControlFlow::Break;
            }
            tries.set(tries.get() + 1);
            if tries.get() > 40 {
                // The node never became linkable; give up so the channel
                // can be rewired without the rack.
                if let Some(h) = &handler {
                    h(FxEvent::CarlaDied(id));
                }
                return glib::ControlFlow::Break;
            }
            let proc = gio::Subprocess::newv(
                &["sh".as_ref(), "-c".as_ref(), script.as_ref()],
                gio::SubprocessFlags::STDOUT_PIPE | gio::SubprocessFlags::STDERR_MERGE,
            );
            if let Ok(proc) = proc {
                let done = done.clone();
                proc.communicate_utf8_async(
                    None,
                    None::<&gio::Cancellable>,
                    move |res| {
                        if let Ok((Some(out), _)) = res {
                            if !out.contains("No such object") {
                                done.set(true);
                            }
                        }
                    },
                );
            }
            glib::ControlFlow::Continue
        });
    }

    // ---- Lifecycle ---------------------------------------------------------

    pub fn remove_channel(&mut self, id: u64) {
        self.ensure_chain(id, None);
        self.kill_carla(id);
        let _ = fs::remove_file(fx_dir().join(format!("ch{id}.conf")));
    }

    pub fn shutdown_all(&mut self) {
        for (_, c) in self.chains.drain() {
            c.slot.kill();
        }
        for (_, c) in self.carlas.drain() {
            c.slot.kill();
        }
    }
}

/// Build the `pipewire -c` configuration hosting this channel's filter
/// chain. Returns None when the channel needs no chain at all.
pub fn chain_conf(id: u64, cfg: &ChannelConfig, catalog: Option<&Catalog>) -> Option<String> {
    if !cfg.fx_active() {
        return None;
    }

    // (declaration lines, [left in, right in], [left out, right out])
    let mut nodes = String::new();
    let mut stages: Vec<([String; 2], [String; 2])> = Vec::new();
    for e in cfg.enabled_effects() {
        let Some(info) = catalog.and_then(|c| c.find(&e.uri)) else {
            // Plugin not installed (anymore): skip it, keep the chain alive.
            continue;
        };
        let uri = sanitize_uri(&e.uri);
        let controls: String = e
            .controls
            .iter()
            .filter(|(s, _)| info.controls.iter().any(|c| c.symbol.as_str() == s.as_str()))
            .map(|(s, v)| format!("\"{}\" = {:.6} ", sanitize_token(s), v))
            .collect();
        let control_block = if controls.is_empty() {
            String::new()
        } else {
            format!(" control = {{ {controls}}}")
        };
        if info.is_mono() {
            let (l, r) = (format!("fx{}_l", e.id), format!("fx{}_r", e.id));
            for n in [&l, &r] {
                nodes.push_str(&format!(
                    "                    {{ type = lv2 name = \"{n}\" plugin = \"{uri}\"{control_block} }}\n"
                ));
            }
            stages.push((
                [
                    format!("{l}:{}", sanitize_token(&info.audio_in[0])),
                    format!("{r}:{}", sanitize_token(&info.audio_in[0])),
                ],
                [
                    format!("{l}:{}", sanitize_token(&info.audio_out[0])),
                    format!("{r}:{}", sanitize_token(&info.audio_out[0])),
                ],
            ));
        } else {
            let n = format!("fx{}", e.id);
            nodes.push_str(&format!(
                "                    {{ type = lv2 name = \"{n}\" plugin = \"{uri}\"{control_block} }}\n"
            ));
            stages.push((
                [
                    format!("{n}:{}", sanitize_token(&info.audio_in[0])),
                    format!("{n}:{}", sanitize_token(&info.audio_in[1])),
                ],
                [
                    format!("{n}:{}", sanitize_token(&info.audio_out[0])),
                    format!("{n}:{}", sanitize_token(&info.audio_out[1])),
                ],
            ));
        }
    }
    if stages.is_empty() {
        // Carla-only (or all plugins missing): pass audio through unchanged.
        for n in ["copy_l", "copy_r"] {
            nodes.push_str(&format!(
                "                    {{ type = builtin name = {n} label = copy }}\n"
            ));
        }
        stages.push((
            ["copy_l:In".to_string(), "copy_r:In".to_string()],
            ["copy_l:Out".to_string(), "copy_r:Out".to_string()],
        ));
    }

    let mut links = String::new();
    for w in stages.windows(2) {
        for ch in 0..2 {
            links.push_str(&format!(
                "                    {{ output = \"{}\" input = \"{}\" }}\n",
                w[0].1[ch], w[1].0[ch]
            ));
        }
    }
    let first = &stages.first().unwrap().0;
    let last = &stages.last().unwrap().1;
    let sink = chain_sink_name(id);
    let source = chain_source_name(id);

    Some(format!(
        r#"# Generated by OpenWave; do not edit (rewritten on every change).
context.properties = {{
    log.level = 2
}}

context.spa-libs = {{
    audio.convert.* = audioconvert/libspa-audioconvert
    support.*       = support/libspa-support
}}

context.modules = [
    {{ name = libpipewire-module-rt
        args = {{ nice.level = -11 }}
        flags = [ ifexists nofail ]
    }}
    {{ name = libpipewire-module-protocol-native }}
    {{ name = libpipewire-module-client-node }}
    {{ name = libpipewire-module-adapter }}

    {{ name = libpipewire-module-filter-chain
        args = {{
            node.description = "OpenWave Channel {id} Effects"
            media.name       = "OpenWave Channel {id} Effects"
            filter.graph = {{
                nodes = [
{nodes}                ]
                links = [
{links}                ]
                inputs  = [ "{in_l}" "{in_r}" ]
                outputs = [ "{out_l}" "{out_r}" ]
            }}
            audio.channels = 2
            audio.position = [ FL FR ]
            capture.props = {{
                node.name           = "{sink}"
                node.description    = "OpenWave Ch {id} FX (internal)"
                media.class         = Audio/Sink
                state.restore-props = false
            }}
            playback.props = {{
                node.name           = "{source}"
                node.description    = "OpenWave Ch {id} FX Out (internal)"
                media.class         = Audio/Source
                state.restore-props = false
            }}
        }}
    }}
]
"#,
        in_l = first[0],
        in_r = first[1],
        out_l = last[0],
        out_r = last[1],
    ))
}

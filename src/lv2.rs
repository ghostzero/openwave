//! LV2 plugin discovery through liblilv, loaded with dlopen at runtime so
//! lilv is an optional dependency: without it the plugin browser is simply
//! unavailable while the rest of the app (and previously configured chains,
//! which PipeWire instantiates itself) keeps working.

use std::cell::OnceCell;
use std::ffi::{c_char, c_void, CStr, CString};
use std::rc::Rc;

use libloading::Library;

/// One input control port of a plugin.
#[derive(Clone, Debug)]
pub struct ControlPort {
    pub symbol: String,
    pub name: String,
    pub min: f32,
    pub max: f32,
    pub default: f32,
    pub toggled: bool,
    pub integer: bool,
}

#[derive(Clone, Debug)]
pub struct PluginInfo {
    pub uri: String,
    pub name: String,
    /// Port symbols. Only 1-in/1-out (instantiated once per stereo channel)
    /// and 2-in/2-out plugins are listed.
    pub audio_in: Vec<String>,
    pub audio_out: Vec<String>,
    pub controls: Vec<ControlPort>,
}

impl PluginInfo {
    pub fn is_mono(&self) -> bool {
        self.audio_in.len() == 1
    }
}

pub struct Catalog {
    pub plugins: Vec<PluginInfo>,
}

impl Catalog {
    pub fn find(&self, uri: &str) -> Option<&PluginInfo> {
        self.plugins.iter().find(|p| p.uri == uri)
    }
}

/// Host features the PipeWire filter-chain LV2 loader provides; plugins
/// requiring anything else are hidden from the browser because they would
/// fail to instantiate.
const SUPPORTED_FEATURES: &[&str] = &[
    "http://lv2plug.in/ns/ext/urid#map",
    "http://lv2plug.in/ns/ext/urid#unmap",
    "http://lv2plug.in/ns/ext/options#options",
    "http://lv2plug.in/ns/ext/worker#schedule",
    "http://lv2plug.in/ns/ext/buf-size#boundedBlockLength",
    "http://lv2plug.in/ns/ext/buf-size#powerOf2BlockLength",
];

type WorldNew = unsafe extern "C" fn() -> *mut c_void;
type WorldOp = unsafe extern "C" fn(*mut c_void);
type WorldGetPlugins = unsafe extern "C" fn(*mut c_void) -> *const c_void;
type NewUri = unsafe extern "C" fn(*mut c_void, *const c_char) -> *mut c_void;
type NodeFree = unsafe extern "C" fn(*mut c_void);
type NodeAsStr = unsafe extern "C" fn(*const c_void) -> *const c_char;
type CollBegin = unsafe extern "C" fn(*const c_void) -> *mut c_void;
type CollNext = unsafe extern "C" fn(*const c_void, *mut c_void) -> *mut c_void;
type CollIsEnd = unsafe extern "C" fn(*const c_void, *mut c_void) -> bool;
type CollGet = unsafe extern "C" fn(*const c_void, *mut c_void) -> *const c_void;
type PluginGetNode = unsafe extern "C" fn(*const c_void) -> *const c_void;
type PluginGetNodeOwned = unsafe extern "C" fn(*const c_void) -> *mut c_void;
type PluginNumPorts = unsafe extern "C" fn(*const c_void) -> u32;
type PluginPortByIndex = unsafe extern "C" fn(*const c_void, u32) -> *const c_void;
type PluginPortRanges =
    unsafe extern "C" fn(*const c_void, *mut f32, *mut f32, *mut f32);
type PortIsA = unsafe extern "C" fn(*const c_void, *const c_void, *const c_void) -> bool;
type PortGetNode =
    unsafe extern "C" fn(*const c_void, *const c_void) -> *const c_void;
type PortGetNodeOwned =
    unsafe extern "C" fn(*const c_void, *const c_void) -> *mut c_void;

struct Api {
    _lib: Library,
    world_new: WorldNew,
    world_load_all: WorldOp,
    world_get_all_plugins: WorldGetPlugins,
    new_uri: NewUri,
    node_free: NodeFree,
    node_as_string: NodeAsStr,
    node_as_uri: NodeAsStr,
    plugins_begin: CollBegin,
    plugins_next: CollNext,
    plugins_is_end: CollIsEnd,
    plugins_get: CollGet,
    nodes_begin: CollBegin,
    nodes_next: CollNext,
    nodes_is_end: CollIsEnd,
    nodes_get: CollGet,
    nodes_free: NodeFree,
    plugin_get_uri: PluginGetNode,
    plugin_get_name: PluginGetNodeOwned,
    plugin_get_num_ports: PluginNumPorts,
    plugin_get_port_by_index: PluginPortByIndex,
    plugin_get_port_ranges_float: PluginPortRanges,
    plugin_get_required_features: PluginGetNodeOwned,
    port_is_a: PortIsA,
    port_get_symbol: PortGetNode,
    port_get_name: PortGetNodeOwned,
    port_has_property: PortIsA,
}

macro_rules! sym {
    ($lib:expr, $name:literal) => {
        *$lib.get(concat!($name, "\0").as_bytes()).ok()?
    };
}

impl Api {
    fn load() -> Option<Self> {
        let lib = ["liblilv-0.so.0", "liblilv-0.so"]
            .iter()
            .find_map(|n| unsafe { Library::new(n) }.ok())?;
        unsafe {
            Some(Self {
                world_new: sym!(lib, "lilv_world_new"),
                world_load_all: sym!(lib, "lilv_world_load_all"),
                world_get_all_plugins: sym!(lib, "lilv_world_get_all_plugins"),
                new_uri: sym!(lib, "lilv_new_uri"),
                node_free: sym!(lib, "lilv_node_free"),
                node_as_string: sym!(lib, "lilv_node_as_string"),
                node_as_uri: sym!(lib, "lilv_node_as_uri"),
                plugins_begin: sym!(lib, "lilv_plugins_begin"),
                plugins_next: sym!(lib, "lilv_plugins_next"),
                plugins_is_end: sym!(lib, "lilv_plugins_is_end"),
                plugins_get: sym!(lib, "lilv_plugins_get"),
                nodes_begin: sym!(lib, "lilv_nodes_begin"),
                nodes_next: sym!(lib, "lilv_nodes_next"),
                nodes_is_end: sym!(lib, "lilv_nodes_is_end"),
                nodes_get: sym!(lib, "lilv_nodes_get"),
                nodes_free: sym!(lib, "lilv_nodes_free"),
                plugin_get_uri: sym!(lib, "lilv_plugin_get_uri"),
                plugin_get_name: sym!(lib, "lilv_plugin_get_name"),
                plugin_get_num_ports: sym!(lib, "lilv_plugin_get_num_ports"),
                plugin_get_port_by_index: sym!(lib, "lilv_plugin_get_port_by_index"),
                plugin_get_port_ranges_float: sym!(lib, "lilv_plugin_get_port_ranges_float"),
                plugin_get_required_features: sym!(lib, "lilv_plugin_get_required_features"),
                port_is_a: sym!(lib, "lilv_port_is_a"),
                port_get_symbol: sym!(lib, "lilv_port_get_symbol"),
                port_get_name: sym!(lib, "lilv_port_get_name"),
                port_has_property: sym!(lib, "lilv_port_has_property"),
                _lib: lib,
            })
        }
    }
}

unsafe fn node_str(api: &Api, node: *const c_void) -> String {
    if node.is_null() {
        return String::new();
    }
    let p = unsafe { (api.node_as_string)(node) };
    if p.is_null() {
        return String::new();
    }
    unsafe { CStr::from_ptr(p) }.to_string_lossy().into_owned()
}

fn build_catalog(api: &Api) -> Catalog {
    let mut plugins = Vec::new();
    unsafe {
        let world = (api.world_new)();
        if world.is_null() {
            return Catalog { plugins };
        }
        (api.world_load_all)(world);

        let uri = |s: &str| {
            let c = CString::new(s).unwrap();
            (api.new_uri)(world, c.as_ptr())
        };
        let audio_class = uri("http://lv2plug.in/ns/lv2core#AudioPort");
        let control_class = uri("http://lv2plug.in/ns/lv2core#ControlPort");
        let input_class = uri("http://lv2plug.in/ns/lv2core#InputPort");
        let output_class = uri("http://lv2plug.in/ns/lv2core#OutputPort");
        let toggled_prop = uri("http://lv2plug.in/ns/lv2core#toggled");
        let integer_prop = uri("http://lv2plug.in/ns/lv2core#integer");
        let supported_uris: Vec<String> = SUPPORTED_FEATURES
            .iter()
            .map(|s| s.to_string())
            .collect();

        let coll = (api.world_get_all_plugins)(world);
        let mut it = (api.plugins_begin)(coll);
        while !(api.plugins_is_end)(coll, it) {
            let plugin = (api.plugins_get)(coll, it);
            it = (api.plugins_next)(coll, it);
            if plugin.is_null() {
                continue;
            }

            // Skip plugins needing host features filter-chain doesn't offer.
            let req = (api.plugin_get_required_features)(plugin);
            let mut ok = true;
            if !req.is_null() {
                let mut fit = (api.nodes_begin)(req);
                while !(api.nodes_is_end)(req, fit) {
                    let f = (api.nodes_get)(req, fit);
                    fit = (api.nodes_next)(req, fit);
                    let furi = if f.is_null() {
                        String::new()
                    } else {
                        let p = (api.node_as_uri)(f);
                        if p.is_null() {
                            String::new()
                        } else {
                            CStr::from_ptr(p).to_string_lossy().into_owned()
                        }
                    };
                    if !supported_uris.iter().any(|s| *s == furi) {
                        ok = false;
                    }
                }
                (api.nodes_free)(req);
            }
            if !ok {
                continue;
            }

            let uri_node = (api.plugin_get_uri)(plugin);
            let plugin_uri = if uri_node.is_null() {
                String::new()
            } else {
                let p = (api.node_as_uri)(uri_node);
                if p.is_null() {
                    String::new()
                } else {
                    CStr::from_ptr(p).to_string_lossy().into_owned()
                }
            };
            if plugin_uri.is_empty() {
                continue;
            }
            let name_node = (api.plugin_get_name)(plugin);
            let name = node_str(api, name_node);
            if !name_node.is_null() {
                (api.node_free)(name_node);
            }
            let name = if name.is_empty() {
                plugin_uri.clone()
            } else {
                name
            };

            let n_ports = (api.plugin_get_num_ports)(plugin);
            let mut mins = vec![f32::NAN; n_ports as usize];
            let mut maxs = vec![f32::NAN; n_ports as usize];
            let mut defs = vec![f32::NAN; n_ports as usize];
            (api.plugin_get_port_ranges_float)(
                plugin,
                mins.as_mut_ptr(),
                maxs.as_mut_ptr(),
                defs.as_mut_ptr(),
            );

            let mut audio_in = Vec::new();
            let mut audio_out = Vec::new();
            let mut controls = Vec::new();
            for i in 0..n_ports {
                let port = (api.plugin_get_port_by_index)(plugin, i);
                if port.is_null() {
                    continue;
                }
                let symbol = node_str(api, (api.port_get_symbol)(plugin, port));
                let is_input = (api.port_is_a)(plugin, port, input_class);
                let is_output = (api.port_is_a)(plugin, port, output_class);
                if (api.port_is_a)(plugin, port, audio_class) {
                    if is_input {
                        audio_in.push(symbol);
                    } else if is_output {
                        audio_out.push(symbol);
                    }
                } else if (api.port_is_a)(plugin, port, control_class) {
                    if !is_input {
                        continue; // output controls are meters/latency reports
                    }
                    let name_node = (api.port_get_name)(plugin, port);
                    let port_name = node_str(api, name_node);
                    if !name_node.is_null() {
                        (api.node_free)(name_node);
                    }
                    let toggled = (api.port_has_property)(plugin, port, toggled_prop);
                    let integer = (api.port_has_property)(plugin, port, integer_prop);
                    let mut min = mins[i as usize];
                    let mut max = maxs[i as usize];
                    let mut def = defs[i as usize];
                    if toggled {
                        min = 0.0;
                        max = 1.0;
                    }
                    if !min.is_finite() {
                        min = 0.0;
                    }
                    if !max.is_finite() || max <= min {
                        max = min + 1.0;
                    }
                    if !def.is_finite() {
                        def = min;
                    }
                    controls.push(ControlPort {
                        name: if port_name.is_empty() {
                            symbol.clone()
                        } else {
                            port_name
                        },
                        symbol,
                        min,
                        max,
                        default: def.clamp(min, max),
                        toggled,
                        integer,
                    });
                }
                // Other port types (Atom, CV) are left unconnected by
                // filter-chain, which the common plugins tolerate; the
                // audio-layout check below is the real gate.
            }

            let stereo_ok = audio_in.len() == 2 && audio_out.len() == 2;
            let mono_ok = audio_in.len() == 1 && audio_out.len() == 1;
            if !(stereo_ok || mono_ok) {
                continue;
            }
            plugins.push(PluginInfo {
                uri: plugin_uri,
                name,
                audio_in,
                audio_out,
                controls,
            });
        }
        // The world is intentionally leaked: catalog strings are copied out,
        // but keeping it alive is harmless and dropping it mid-session buys
        // nothing (lilv has no unload path for us to exercise safely).
    }
    plugins.sort_by(|a, b| a.name.to_lowercase().cmp(&b.name.to_lowercase()));
    Catalog { plugins }
}

thread_local! {
    static CATALOG: OnceCell<Option<Rc<Catalog>>> = const { OnceCell::new() };
}

/// Whether liblilv could be loaded at all (decides if the plugin browser is
/// offered). Cheap after the first call.
pub fn available() -> bool {
    catalog().is_some()
}

/// The installed-plugin catalog, scanned once per process on first use.
pub fn catalog() -> Option<Rc<Catalog>> {
    CATALOG.with(|c| {
        c.get_or_init(|| Api::load().map(|api| Rc::new(build_catalog(&api))))
            .clone()
    })
}

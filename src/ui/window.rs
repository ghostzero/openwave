use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::rc::Rc;
use std::time::{Duration, Instant};

use adw::prelude::*;
use gtk::{gio, glib};

use crate::audio::{AudioEvent, LevelTarget, Mix, PulseManager};
use crate::config::{Assignment, Config, MAX_CHANNELS};

use super::channel_strip::ChannelStrip;
use super::effects::{self, EffectsDeps};
use super::heading_label;
use super::outputs::OutputsPanel;
use super::setup;
use super::sidebar::Sidebar;
use super::wave_xlr;

struct App {
    config: Rc<RefCell<Config>>,
    manager: PulseManager,
    window: glib::WeakRef<adw::ApplicationWindow>,
    toasts: adw::ToastOverlay,
    /// Channel strips currently shown, each paired with its channel id.
    strips: RefCell<Vec<(u64, ChannelStrip)>>,
    strips_box: gtk::Box,
    add_button: gtk::MenuButton,
    add_menu: gio::Menu,
    outputs: OutputsPanel,
    sidebar: Sidebar,
    stack: gtk::Stack,
    error_page: adw::StatusPage,
    save_pending: Cell<bool>,
    /// Per-channel edit counter used to debounce rename → sink rebuild.
    rename_epoch: RefCell<HashMap<u64, u64>>,
    /// The open effects dialog (channel id + hooks), so VST rack load
    /// results and native-UI parameter edits can update it live.
    fx_dialog: RefCell<Option<(u64, Rc<effects::DialogHooks>)>>,
    /// Refresh hook of the open setup dialog, driven by device changes.
    setup_hook: RefCell<Option<Rc<dyn Fn()>>>,
    /// First-run dialog / misconfiguration notice already handled this
    /// session.
    setup_prompted: Cell<bool>,
    /// Wave XLR volumes were restored for the current device appearance;
    /// reset when the device disappears so a replug restores them again.
    /// Enforcement deadlines for the stored Wave XLR startup volumes; None
    /// while the device is absent, set on each appearance.
    xlr_mic_hold: Cell<Option<Instant>>,
    xlr_out_hold: Cell<Option<Instant>>,
    /// Forces every strip and the add-channel card to the same width.
    strip_size_group: gtk::SizeGroup,
}

pub fn build(application: &adw::Application) -> adw::ApplicationWindow {
    let config = Rc::new(RefCell::new(Config::load()));
    let manager = PulseManager::new(config.clone());
    let outputs = OutputsPanel::new();
    let sidebar = Sidebar::new();

    outputs.load_config(&config.borrow().master);
    outputs.set_vod_visible(config.borrow().vod_mix_enabled);

    // ---- Mixer page ----------------------------------------------------------
    // Strips keep a fixed width; the row scrolls instead of stretching.
    let strips_box = gtk::Box::new(gtk::Orientation::Horizontal, 12);
    strips_box.set_halign(gtk::Align::Start);
    let strips_scroller = gtk::ScrolledWindow::builder()
        .hscrollbar_policy(gtk::PolicyType::Automatic)
        .vscrollbar_policy(gtk::PolicyType::Never)
        .child(&strips_box)
        .vexpand(true)
        .build();

    let add_menu = gio::Menu::new();
    let add_content = gtk::Box::new(gtk::Orientation::Vertical, 8);
    add_content.set_valign(gtk::Align::Center);
    let add_icon = gtk::Image::from_icon_name("list-add-symbolic");
    add_icon.set_pixel_size(24);
    add_content.append(&add_icon);
    let add_label = gtk::Label::new(Some("Add Channel"));
    add_label.add_css_class("dim-label");
    add_content.append(&add_label);
    let add_button = gtk::MenuButton::builder()
        .child(&add_content)
        .menu_model(&add_menu)
        .tooltip_text("Add a channel")
        .width_request(super::channel_strip::STRIP_WIDTH)
        .build();
    add_button.add_css_class("card");
    add_button.add_css_class("flat");

    let strip_size_group = gtk::SizeGroup::new(gtk::SizeGroupMode::Horizontal);
    strip_size_group.add_widget(&add_button);

    let mixer = gtk::Box::builder()
        .orientation(gtk::Orientation::Vertical)
        .spacing(12)
        .margin_top(12)
        .margin_bottom(16)
        .margin_start(16)
        .margin_end(16)
        .build();
    mixer.append(&heading_label("Inputs"));
    mixer.append(&strips_scroller);
    mixer.append(&heading_label("Outputs"));
    mixer.append(&outputs.root);

    let clamp = adw::Clamp::builder()
        .maximum_size(1600)
        .child(&mixer)
        .build();

    // ---- Status pages ----------------------------------------------------------
    let spinner = adw::Spinner::new();
    spinner.set_size_request(32, 32);
    spinner.set_halign(gtk::Align::Center);
    let connecting_page = adw::StatusPage::builder()
        .icon_name("audio-card-symbolic")
        .title("Connecting to Audio Server")
        .description("Creating the virtual mix devices…")
        .child(&spinner)
        .build();

    let retry = gtk::Button::builder()
        .label("Retry")
        .halign(gtk::Align::Center)
        .css_classes(["pill", "suggested-action"])
        .build();
    let error_page = adw::StatusPage::builder()
        .icon_name("audio-volume-muted-symbolic")
        .title("Audio Server Unavailable")
        .child(&retry)
        .build();

    let stack = gtk::Stack::builder()
        .transition_type(gtk::StackTransitionType::Crossfade)
        .build();
    stack.add_named(&connecting_page, Some("connecting"));
    stack.add_named(&error_page, Some("error"));
    stack.add_named(&clamp, Some("mixer"));

    // ---- Window chrome ----------------------------------------------------------
    let header = adw::HeaderBar::builder()
        .title_widget(&adw::WindowTitle::new("OpenWave", "Dual-Mix Audio Router"))
        .build();

    let menu = gio::Menu::new();
    let menu_settings = gio::Menu::new();
    menu_settings.append(Some("Audio Setup…"), Some("win.setup"));
    menu_settings.append(Some("Wave XLR…"), Some("win.wave-xlr"));
    menu_settings.append(Some("Enable VOD Mix"), Some("win.vod-mix"));
    menu_settings.append(Some("Start at Login"), Some("win.autostart"));
    menu.append_section(None, &menu_settings);
    let menu_general = gio::Menu::new();
    menu_general.append(Some("About"), Some("win.about"));
    menu_general.append(Some("Quit"), Some("app.quit"));
    menu.append_section(None, &menu_general);
    let menu_button = gtk::MenuButton::builder()
        .icon_name("open-menu-symbolic")
        .menu_model(&menu)
        .primary(true)
        .tooltip_text("Main Menu")
        .build();
    header.pack_end(&menu_button);

    let sidebar_toggle = gtk::ToggleButton::builder()
        .icon_name("sidebar-show-right-symbolic")
        .tooltip_text("Show Devices")
        .build();
    header.pack_end(&sidebar_toggle);

    let toasts = adw::ToastOverlay::new();
    toasts.set_child(Some(&stack));

    let toolbar = adw::ToolbarView::new();
    toolbar.add_top_bar(&header);
    toolbar.set_content(Some(&toasts));

    let split = adw::OverlaySplitView::builder()
        .content(&toolbar)
        .sidebar(&sidebar.root)
        .sidebar_position(gtk::PackType::End)
        .show_sidebar(false)
        .max_sidebar_width(320.0)
        .build();
    sidebar_toggle
        .bind_property("active", &split, "show-sidebar")
        .bidirectional()
        .sync_create()
        .build();

    let window = adw::ApplicationWindow::builder()
        .application(application)
        .title("OpenWave")
        .default_width(1280)
        .default_height(720)
        .content(&split)
        .build();

    let app = Rc::new(App {
        config,
        manager,
        window: window.downgrade(),
        toasts,
        strips: RefCell::new(Vec::new()),
        strips_box,
        add_button,
        add_menu,
        outputs,
        sidebar,
        stack,
        error_page,
        save_pending: Cell::new(false),
        rename_epoch: RefCell::new(HashMap::new()),
        fx_dialog: RefCell::new(None),
        setup_hook: RefCell::new(None),
        setup_prompted: Cell::new(false),
        xlr_mic_hold: Cell::new(None),
        xlr_out_hold: Cell::new(None),
        strip_size_group,
    });

    wire_actions(&app, &window);
    wire_outputs(&app);
    wire_audio_events(&app);
    {
        let app = app.clone();
        retry.connect_clicked(move |_| {
            app.stack.set_visible_child_name("connecting");
            app.manager.connect_server();
        });
    }
    wire_close(&app, &window);
    wire_quit(&app, application, &window);

    // A window started with --hidden gets its setup check when it is first
    // shown instead of at startup.
    {
        let app = app.clone();
        window.connect_map(move |_| {
            let app = app.clone();
            glib::timeout_add_local_once(Duration::from_secs(1), move || {
                maybe_prompt_setup(&app);
            });
        });
    }

    rebuild_strips(&app);
    app.manager.connect_server();
    window
}

// ---- Persistence -----------------------------------------------------------

fn schedule_save(app: &Rc<App>) {
    if app.save_pending.replace(true) {
        return;
    }
    let app = app.clone();
    glib::timeout_add_local_once(Duration::from_millis(700), move || {
        app.save_pending.set(false);
        app.config.borrow().save();
    });
}

// ---- Reactions to audio-server events ---------------------------------------

fn wire_audio_events(app: &Rc<App>) {
    let weak = Rc::downgrade(app);
    app.manager.set_event_handler(move |ev| {
        let Some(app) = weak.upgrade() else {
            return;
        };
        match ev {
            AudioEvent::Ready => {
                app.stack.set_visible_child_name("mixer");
                // Give the buses/loopbacks a moment to settle before judging
                // the setup, so startup churn doesn't read as misconfigured.
                let app = app.clone();
                glib::timeout_add_local_once(Duration::from_secs(3), move || {
                    maybe_prompt_setup(&app);
                });
            }
            AudioEvent::Failed(msg) => {
                app.error_page.set_description(Some(&msg));
                app.stack.set_visible_child_name("error");
            }
            AudioEvent::DevicesChanged => {
                refresh_devices(&app);
                restore_wave_xlr(&app);
                let hook = app.setup_hook.borrow().clone();
                if let Some(refresh) = hook {
                    refresh();
                }
            }
            AudioEvent::Level(target, v) => match target {
                LevelTarget::Channel(id) => {
                    if let Some((_, strip)) =
                        app.strips.borrow().iter().find(|(cid, _)| *cid == id)
                    {
                        strip.set_levels(&v);
                    }
                }
                LevelTarget::MonitorMix => app.outputs.monitor_level.set_levels(&v),
                LevelTarget::StreamMix => app.outputs.stream_level.set_levels(&v),
                LevelTarget::VodMix => app.outputs.vod_level.set_levels(&v),
            },
            AudioEvent::VstChanged(id) => {
                let hooks = app
                    .fx_dialog
                    .borrow()
                    .as_ref()
                    .filter(|(did, _)| *did == id)
                    .map(|(_, h)| h.clone());
                if let Some(hooks) = hooks {
                    (hooks.refresh)();
                }
            }
            AudioEvent::VstParams(id, updates) => {
                schedule_save(&app);
                let hooks = app
                    .fx_dialog
                    .borrow()
                    .as_ref()
                    .filter(|(did, _)| *did == id)
                    .map(|(_, h)| h.clone());
                if let Some(hooks) = hooks {
                    (hooks.sync_params)(&updates);
                }
            }
        }
    });
}

fn refresh_devices(app: &Rc<App>) {
    let sources = app.manager.sources();
    let apps = app.manager.app_names();

    let mut items: Vec<(String, Option<Assignment>)> = vec![
        ("No Input".to_string(), None),
        ("Virtual Device".to_string(), Some(Assignment::Virtual)),
    ];
    for s in sources.iter().filter(|s| !s.is_monitor) {
        items.push((
            s.description.clone(),
            Some(Assignment::Source {
                name: s.name.clone(),
            }),
        ));
    }
    for a in &apps {
        items.push((
            format!("{a} — Application"),
            Some(Assignment::App { name: a.clone() }),
        ));
    }
    for s in sources.iter().filter(|s| s.is_monitor) {
        items.push((
            s.description.clone(),
            Some(Assignment::Source {
                name: s.name.clone(),
            }),
        ));
    }

    let sinks = app.manager.output_sinks();
    let mut sink_items: Vec<(String, Option<String>)> =
        vec![("System Default".to_string(), None)];
    for s in &sinks {
        sink_items.push((s.description.clone(), Some(s.name.clone())));
    }

    {
        let cfg = app.config.borrow();
        for (id, strip) in app.strips.borrow().iter() {
            if let Some(ch) = cfg.channel(*id) {
                strip.set_input_entries(&items, &ch.assignment);
            }
        }
        app.outputs
            .set_output_sinks(&sink_items, &cfg.master.monitor_device);
    }
    update_sidebar(app);
}

fn update_sidebar(app: &Rc<App>) {
    let cfg = app.config.borrow().clone();
    let sinks = app.manager.output_sinks();
    let monitor_label = cfg
        .master
        .monitor_device
        .as_ref()
        .and_then(|d| {
            sinks
                .iter()
                .find(|s| &s.name == d)
                .map(|s| s.description.clone())
        })
        .unwrap_or_else(|| "system default output".to_string());
    let manager = app.manager.clone();
    app.sidebar.update(&cfg, &monitor_label, &move |a| match a {
        Assignment::Source { name } => manager
            .source_description(name)
            .unwrap_or_else(|| name.clone()),
        Assignment::App { name } => format!("{name} — application"),
        Assignment::Virtual => "Virtual device".to_string(),
    });
}

// ---- Channel strips (dynamic) -------------------------------------------------

/// Recreate all channel strips from the config. Called on startup and after
/// adding/removing a channel.
fn rebuild_strips(app: &Rc<App>) {
    while let Some(child) = app.strips_box.first_child() {
        app.strips_box.remove(&child);
    }
    let channels = app.config.borrow().channels.clone();
    let vod = app.config.borrow().vod_mix_enabled;
    let mut strips = Vec::with_capacity(channels.len());
    for ch in &channels {
        let strip = ChannelStrip::new();
        strip.load_config(ch);
        strip.set_vod_visible(vod);
        if ch.permanent {
            // Shown but disabled: permanent strips keep the exact same
            // header layout as removable ones.
            strip.remove.set_sensitive(false);
            strip.remove.set_tooltip_text(Some("Built-in channels cannot be removed"));
        }
        wire_strip(app, &strip, ch.id);
        app.strips_box.append(&strip.root);
        app.strip_size_group.add_widget(&strip.root);
        strips.push((ch.id, strip));
    }
    app.strips_box.append(&app.add_button);
    app.add_button.set_visible(channels.len() < MAX_CHANNELS);
    *app.strips.borrow_mut() = strips;
    rebuild_add_menu(app);
    refresh_devices(app);
}

fn rebuild_add_menu(app: &Rc<App>) {
    app.add_menu.remove_all();
    let templates = app.config.borrow().unused_template_names();
    for name in templates {
        app.add_menu
            .append(Some(name), Some(&format!("win.add-channel('{name}')")));
    }
    app.add_menu
        .append(Some("Custom Channel"), Some("win.add-channel('')"));
}

fn wire_strip(app: &Rc<App>, strip: &ChannelStrip, id: u64) {
    {
        let app = app.clone();
        let guard = strip.guard.clone();
        strip.name.connect_changed(move |editable| {
            if guard.get() {
                return;
            }
            let text = editable.text().to_string();
            {
                let mut cfg = app.config.borrow_mut();
                let Some(ch) = cfg.channel_mut(id) else {
                    return;
                };
                ch.name = text;
            }
            schedule_save(&app);
            update_sidebar(&app);
            rebuild_add_menu(&app);
            // Virtual/App channels expose a device named after the channel;
            // rebuild the channel sink once the user stops typing.
            let epoch = {
                let mut epochs = app.rename_epoch.borrow_mut();
                let e = epochs.entry(id).or_insert(0);
                *e += 1;
                *e
            };
            let app = app.clone();
            glib::timeout_add_local_once(Duration::from_millis(900), move || {
                if app.rename_epoch.borrow().get(&id) != Some(&epoch) {
                    return;
                }
                let needs_sink = matches!(
                    app.config.borrow().channel(id).and_then(|c| c.assignment.clone()),
                    Some(Assignment::App { .. }) | Some(Assignment::Virtual)
                );
                if needs_sink {
                    app.manager.rebuild_channel(id);
                }
            });
        });
    }

    {
        let app = app.clone();
        let guard = strip.guard.clone();
        let entries = strip.entries.clone();
        strip.input.connect_selected_notify(move |dd| {
            if guard.get() {
                return;
            }
            let assignment = entries
                .borrow()
                .get(dd.selected() as usize)
                .cloned()
                .flatten();
            {
                let mut cfg = app.config.borrow_mut();
                let Some(ch) = cfg.channel_mut(id) else {
                    return;
                };
                if ch.assignment == assignment {
                    return;
                }
                ch.assignment = assignment;
            }
            app.manager.rebuild_channel(id);
            schedule_save(&app);
            update_sidebar(&app);
        });
    }

    {
        let app = app.clone();
        let guard = strip.guard.clone();
        let others = [strip.stream_scale.clone(), strip.vod_scale.clone()];
        strip.monitor_scale.connect_value_changed(move |scale| {
            if guard.get() {
                return;
            }
            let v = scale.value();
            let linked = {
                let mut cfg = app.config.borrow_mut();
                let Some(ch) = cfg.channel_mut(id) else {
                    return;
                };
                ch.monitor_volume = v;
                ch.linked
            };
            app.manager.apply_channel_mix(id, Mix::Monitor);
            if linked {
                for other in &others {
                    other.set_value(v);
                }
            }
            schedule_save(&app);
        });
    }

    {
        let app = app.clone();
        let guard = strip.guard.clone();
        let others = [strip.monitor_scale.clone(), strip.vod_scale.clone()];
        strip.stream_scale.connect_value_changed(move |scale| {
            if guard.get() {
                return;
            }
            let v = scale.value();
            let linked = {
                let mut cfg = app.config.borrow_mut();
                let Some(ch) = cfg.channel_mut(id) else {
                    return;
                };
                ch.stream_volume = v;
                ch.linked
            };
            app.manager.apply_channel_mix(id, Mix::Stream);
            if linked {
                for other in &others {
                    other.set_value(v);
                }
            }
            schedule_save(&app);
        });
    }

    {
        let app = app.clone();
        let guard = strip.guard.clone();
        let others = [strip.monitor_scale.clone(), strip.stream_scale.clone()];
        strip.vod_scale.connect_value_changed(move |scale| {
            if guard.get() {
                return;
            }
            let v = scale.value();
            let linked = {
                let mut cfg = app.config.borrow_mut();
                let Some(ch) = cfg.channel_mut(id) else {
                    return;
                };
                ch.vod_volume = v;
                ch.linked
            };
            app.manager.apply_channel_mix(id, Mix::Vod);
            if linked {
                for other in &others {
                    other.set_value(v);
                }
            }
            schedule_save(&app);
        });
    }

    {
        let app = app.clone();
        let guard = strip.guard.clone();
        strip.monitor_mute.connect_toggled(move |btn| {
            if guard.get() {
                return;
            }
            {
                let mut cfg = app.config.borrow_mut();
                let Some(ch) = cfg.channel_mut(id) else {
                    return;
                };
                ch.monitor_muted = btn.is_active();
            }
            app.manager.apply_channel_mix(id, Mix::Monitor);
            schedule_save(&app);
        });
    }

    {
        let app = app.clone();
        let guard = strip.guard.clone();
        strip.stream_mute.connect_toggled(move |btn| {
            if guard.get() {
                return;
            }
            {
                let mut cfg = app.config.borrow_mut();
                let Some(ch) = cfg.channel_mut(id) else {
                    return;
                };
                ch.stream_muted = btn.is_active();
            }
            app.manager.apply_channel_mix(id, Mix::Stream);
            schedule_save(&app);
        });
    }

    {
        let app = app.clone();
        let guard = strip.guard.clone();
        strip.vod_mute.connect_toggled(move |btn| {
            if guard.get() {
                return;
            }
            {
                let mut cfg = app.config.borrow_mut();
                let Some(ch) = cfg.channel_mut(id) else {
                    return;
                };
                ch.vod_muted = btn.is_active();
            }
            app.manager.apply_channel_mix(id, Mix::Vod);
            schedule_save(&app);
        });
    }

    {
        let app = app.clone();
        let guard = strip.guard.clone();
        let monitor_scale = strip.monitor_scale.clone();
        let stream_scale = strip.stream_scale.clone();
        let vod_scale = strip.vod_scale.clone();
        strip.link.connect_toggled(move |btn| {
            if guard.get() {
                return;
            }
            {
                let mut cfg = app.config.borrow_mut();
                let Some(ch) = cfg.channel_mut(id) else {
                    return;
                };
                ch.linked = btn.is_active();
            }
            if btn.is_active() {
                stream_scale.set_value(monitor_scale.value());
                vod_scale.set_value(monitor_scale.value());
            }
            schedule_save(&app);
        });
    }

    {
        let app = app.clone();
        strip.fx.connect_clicked(move |btn| {
            let on_structure = {
                let app = app.clone();
                Rc::new(move |id: u64| {
                    app.manager.rebuild_channel(id);
                    schedule_save(&app);
                    update_sidebar(&app);
                    let cfg = app.config.borrow();
                    if let Some(ch) = cfg.channel(id)
                        && let Some((_, strip)) =
                            app.strips.borrow().iter().find(|(cid, _)| *cid == id)
                        {
                            strip.update_fx_indicator(ch);
                        }
                })
            };
            let on_control = {
                let app = app.clone();
                Rc::new(move |_id: u64| schedule_save(&app))
            };
            let (dialog, hooks) = effects::open(
                btn,
                EffectsDeps {
                    config: app.config.clone(),
                    manager: app.manager.clone(),
                    on_structure,
                    on_control,
                },
                id,
            );
            *app.fx_dialog.borrow_mut() = Some((id, Rc::new(hooks)));
            let app = app.clone();
            dialog.connect_closed(move |_| {
                let mut open = app.fx_dialog.borrow_mut();
                if open.as_ref().is_some_and(|(did, _)| *did == id) {
                    *open = None;
                }
            });
        });
    }

    {
        let app = app.clone();
        strip.remove.connect_clicked(move |btn| {
            let name = app
                .config
                .borrow()
                .channel(id)
                .map(|c| c.name.clone())
                .unwrap_or_default();
            let confirm = adw::AlertDialog::builder()
                .heading("Remove Channel?")
                .body(format!(
                    "“{name}” will be removed, along with its input assignment, \
                     mix levels and effects."
                ))
                .default_response("cancel")
                .close_response("cancel")
                .build();
            confirm.add_responses(&[("cancel", "Cancel"), ("remove", "Remove")]);
            confirm.set_response_appearance("remove", adw::ResponseAppearance::Destructive);
            let app = app.clone();
            confirm.connect_response(Some("remove"), move |_, _| {
                app.config.borrow_mut().remove_channel(id);
                app.manager.rebuild_channel(id);
                rebuild_strips(&app);
                schedule_save(&app);
                update_sidebar(&app);
            });
            confirm.present(Some(btn));
        });
    }
}

// ---- Outputs section -----------------------------------------------------------

fn wire_outputs(app: &Rc<App>) {
    {
        let app = app.clone();
        app.clone()
            .outputs
            .monitor_device
            .connect_selected_notify(move |dd| {
                if app.outputs.guard.get() {
                    return;
                }
                let device = app
                    .outputs
                    .sink_entries
                    .borrow()
                    .get(dd.selected() as usize)
                    .cloned()
                    .flatten();
                {
                    let mut cfg = app.config.borrow_mut();
                    if cfg.master.monitor_device == device {
                        return;
                    }
                    cfg.master.monitor_device = device;
                }
                app.manager.setup_monitor_output();
                schedule_save(&app);
                update_sidebar(&app);
            });
    }
    {
        let app = app.clone();
        app.clone()
            .outputs
            .monitor_scale
            .connect_value_changed(move |scale| {
                if app.outputs.guard.get() {
                    return;
                }
                app.config.borrow_mut().master.monitor_volume = scale.value();
                app.manager.apply_master_monitor();
                schedule_save(&app);
            });
    }
    {
        let app = app.clone();
        app.clone()
            .outputs
            .monitor_mute
            .connect_toggled(move |btn| {
                if app.outputs.guard.get() {
                    return;
                }
                app.config.borrow_mut().master.monitor_muted = btn.is_active();
                app.manager.apply_master_monitor();
                schedule_save(&app);
            });
    }
    {
        let app = app.clone();
        app.clone()
            .outputs
            .stream_scale
            .connect_value_changed(move |scale| {
                if app.outputs.guard.get() {
                    return;
                }
                app.config.borrow_mut().master.stream_volume = scale.value();
                app.manager.apply_master_stream();
                schedule_save(&app);
            });
    }
    {
        let app = app.clone();
        app.clone().outputs.stream_mute.connect_toggled(move |btn| {
            if app.outputs.guard.get() {
                return;
            }
            app.config.borrow_mut().master.stream_muted = btn.is_active();
            app.manager.apply_master_stream();
            schedule_save(&app);
        });
    }
    {
        let app = app.clone();
        app.clone()
            .outputs
            .vod_scale
            .connect_value_changed(move |scale| {
                if app.outputs.guard.get() {
                    return;
                }
                app.config.borrow_mut().master.vod_volume = scale.value();
                app.manager.apply_master_vod();
                schedule_save(&app);
            });
    }
    {
        let app = app.clone();
        app.clone().outputs.vod_mute.connect_toggled(move |btn| {
            if app.outputs.guard.get() {
                return;
            }
            app.config.borrow_mut().master.vod_muted = btn.is_active();
            app.manager.apply_master_vod();
            schedule_save(&app);
        });
    }
}

// ---- Setup assistant --------------------------------------------------------

/// First run: present the setup assistant. Later runs: if the system audio
/// drifted away from the recommended setup, show a notice with a shortcut to
/// the assistant. Runs at most once per session, and only while the window
/// is actually visible.
fn maybe_prompt_setup(app: &Rc<App>) {
    if app.setup_prompted.get() {
        return;
    }
    let Some(window) = app.window.upgrade() else {
        return;
    };
    if !window.is_visible()
        || app.stack.visible_child_name().as_deref() != Some("mixer")
    {
        return;
    }
    let (first_run, all_ok) = {
        let cfg = app.config.borrow();
        (!cfg.setup_done, setup::all_ok(&cfg, &app.manager))
    };
    app.setup_prompted.set(true);
    if first_run {
        app.config.borrow_mut().setup_done = true;
        schedule_save(app);
        open_setup(app);
    } else if !all_ok {
        let toast = adw::Toast::builder()
            .title("The system audio setup needs attention")
            .button_label("Review")
            .timeout(0)
            .build();
        {
            let app = app.clone();
            toast.connect_button_clicked(move |_| open_setup(&app));
        }
        app.toasts.add_toast(toast);
    }
}

fn open_setup(app: &Rc<App>) {
    if app.setup_hook.borrow().is_some() {
        return;
    }
    let Some(window) = app.window.upgrade() else {
        return;
    };
    let on_changed: Rc<dyn Fn()> = {
        let app = app.clone();
        Rc::new(move || {
            schedule_save(&app);
            refresh_devices(&app);
            update_sidebar(&app);
        })
    };
    let (dialog, refresh) = setup::open(
        &window,
        setup::SetupDeps {
            config: app.config.clone(),
            manager: app.manager.clone(),
            on_changed,
        },
    );
    *app.setup_hook.borrow_mut() = Some(refresh);
    let app = app.clone();
    dialog.connect_closed(move |_| {
        *app.setup_hook.borrow_mut() = None;
    });
}

// ---- Wave XLR ---------------------------------------------------------------

fn open_wave_xlr(app: &Rc<App>) {
    let Some(window) = app.window.upgrade() else {
        return;
    };
    let on_changed: Rc<dyn Fn()> = {
        let app = app.clone();
        Rc::new(move || schedule_save(&app))
    };
    wave_xlr::open(
        &window,
        wave_xlr::XlrDeps {
            config: app.config.clone(),
            manager: app.manager.clone(),
            on_changed,
        },
    );
}

/// How long the stored Wave XLR startup volumes are enforced after startup
/// or a device (re)appearance. A single write is not enough: the device's
/// node suspends/resumes while the channels wire up, WirePlumber re-applies
/// its own stored route volumes on activation, and the firmware itself
/// occasionally resets to 100% — whichever write lands last wins, so during
/// this window every refresh that shows a drifted volume re-applies ours
/// (each reset raises a change event, so no polling is needed). Afterwards
/// the device's physical controls are left alone.
const XLR_ENFORCE_WINDOW: Duration = Duration::from_secs(15);

fn restore_wave_xlr(app: &Rc<App>) {
    let (mic, out) = {
        let cfg = app.config.borrow();
        (cfg.wave_xlr.mic_volume, cfg.wave_xlr.output_volume)
    };
    let mic_dev = app.manager.wave_xlr_source().map(|s| (s.name, s.volume));
    let out_dev = app.manager.wave_xlr_sink().map(|s| (s.name, s.volume));
    enforce_xlr_volume(mic_dev, mic, &app.xlr_mic_hold, |name, pct| {
        app.manager.set_source_volume(name, pct);
    });
    enforce_xlr_volume(out_dev, out, &app.xlr_out_hold, |name, pct| {
        app.manager.set_sink_volume(name, pct);
    });
}

fn enforce_xlr_volume(
    dev: Option<(String, f64)>,
    stored: Option<f64>,
    hold: &Cell<Option<Instant>>,
    apply: impl Fn(&str, f64),
) {
    let Some((name, current)) = dev else {
        hold.set(None);
        return;
    };
    let deadline = match hold.get() {
        Some(d) => d,
        None => {
            let d = Instant::now() + XLR_ENFORCE_WINDOW;
            hold.set(Some(d));
            d
        }
    };
    if Instant::now() > deadline {
        return;
    }
    if let Some(pct) = stored
        && (current - pct).abs() > 1.0
    {
        apply(&name, pct);
    }
}

// ---- Window actions -------------------------------------------------------------

// ---- Autostart -------------------------------------------------------------

fn autostart_file() -> std::path::PathBuf {
    glib::user_config_dir()
        .join("autostart")
        .join(format!("{}.desktop", crate::APP_ID))
}

fn autostart_enabled() -> bool {
    autostart_file().exists()
}

/// Enable/disable launch-on-login via an XDG autostart entry. The entry runs
/// the current executable with --hidden so the virtual devices come up in
/// the background without opening the window.
fn set_autostart(enable: bool) -> std::io::Result<()> {
    let path = autostart_file();
    if !enable {
        if path.exists() {
            std::fs::remove_file(&path)?;
        }
        return Ok(());
    }
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let exe = std::env::current_exe()
        .map(|p| p.display().to_string())
        .unwrap_or_else(|_| "openwave".to_string());
    std::fs::write(
        &path,
        format!(
            "[Desktop Entry]\n\
             Type=Application\n\
             Name=OpenWave\n\
             Comment=Dual-mix virtual audio mixer for streaming\n\
             Exec={exe} --hidden\n\
             Icon={}\n\
             Terminal=false\n\
             X-GNOME-Autostart-enabled=true\n",
            crate::APP_ID
        ),
    )
}

fn wire_actions(app: &Rc<App>, window: &adw::ApplicationWindow) {
    let autostart = gio::SimpleAction::new_stateful(
        "autostart",
        None,
        &autostart_enabled().to_variant(),
    );
    autostart.connect_activate(|action, _| {
        let enable = !action
            .state()
            .and_then(|s| s.get::<bool>())
            .unwrap_or(false);
        match set_autostart(enable) {
            Ok(()) => action.set_state(&enable.to_variant()),
            Err(e) => eprintln!("openwave: could not update autostart entry: {e}"),
        }
    });
    window.add_action(&autostart);

    let add = gio::SimpleAction::new("add-channel", Some(glib::VariantTy::STRING));
    {
        let app = app.clone();
        add.connect_activate(move |_, param| {
            let name = param.and_then(|v| v.str()).unwrap_or("");
            let name = if name.is_empty() { None } else { Some(name) };
            let id = app.config.borrow_mut().add_channel(name);
            if let Some(id) = id {
                app.manager.rebuild_channel(id);
                rebuild_strips(&app);
                schedule_save(&app);
                update_sidebar(&app);
            }
        });
    }
    window.add_action(&add);

    let vod = gio::SimpleAction::new_stateful(
        "vod-mix",
        None,
        &app.config.borrow().vod_mix_enabled.to_variant(),
    );
    {
        let app = app.clone();
        vod.connect_activate(move |action, _| {
            let enable = !action
                .state()
                .and_then(|s| s.get::<bool>())
                .unwrap_or(false);
            action.set_state(&enable.to_variant());
            app.config.borrow_mut().vod_mix_enabled = enable;
            app.manager.apply_vod_mix();
            app.outputs.set_vod_visible(enable);
            for (_, strip) in app.strips.borrow().iter() {
                strip.set_vod_visible(enable);
            }
            schedule_save(&app);
            update_sidebar(&app);
        });
    }
    window.add_action(&vod);

    let setup_action = gio::SimpleAction::new("setup", None);
    {
        let app = app.clone();
        setup_action.connect_activate(move |_, _| open_setup(&app));
    }
    window.add_action(&setup_action);

    let xlr_action = gio::SimpleAction::new("wave-xlr", None);
    {
        let app = app.clone();
        xlr_action.connect_activate(move |_, _| open_wave_xlr(&app));
    }
    window.add_action(&xlr_action);

    let about = gio::SimpleAction::new("about", None);
    let win_weak = window.downgrade();
    about.connect_activate(move |_, _| {
        if let Some(win) = win_weak.upgrade() {
            let dialog = adw::AboutDialog::builder()
                .application_name("OpenWave")
                .application_icon("de.ghostzero.OpenWave")
                .developer_name("René Preuß")
                .copyright("© 2026 René Preuß")
                .version(env!("CARGO_PKG_VERSION"))
                .comments(
                    "Dual-mix virtual audio mixer for Linux. \
                     Route hardware inputs and applications into independent \
                     monitor and stream mixes.",
                )
                .license_type(gtk::License::MitX11)
                .build();
            dialog.present(Some(&win));
        }
    });
    window.add_action(&about);
}

/// Closing the window only hides it: the virtual devices and all routing
/// keep working in the background. Launching the app again (or activating
/// it from the shell) brings the window back; "Quit" tears everything down.
fn wire_close(app: &Rc<App>, window: &adw::ApplicationWindow) {
    let app = app.clone();
    let notified = Cell::new(false);
    window.connect_close_request(move |win| {
        app.config.borrow().save();
        win.set_visible(false);
        if !notified.replace(true)
            && let Some(gapp) = win.application() {
                let note = gio::Notification::new("OpenWave is still running");
                note.set_body(Some(
                    "The virtual audio devices stay active in the background. \
                     Use Quit in the main menu to stop them.",
                ));
                gapp.send_notification(Some("openwave-background"), &note);
            }
        glib::Propagation::Stop
    });
}

/// app.quit: save, unload everything we created on the audio server, then
/// really exit.
fn wire_quit(app: &Rc<App>, application: &adw::Application, window: &adw::ApplicationWindow) {
    let quit = gio::SimpleAction::new("quit", None);
    let outer_application = application.clone();
    let app = app.clone();
    let application = application.clone();
    let window = window.clone();
    quit.connect_activate(move |_, _| {
        app.config.borrow().save();
        let done = Rc::new(Cell::new(false));
        let finish = {
            let application = application.clone();
            let window = window.clone();
            move || {
                window.destroy();
                application.quit();
            }
        };
        {
            let done = done.clone();
            let finish = finish.clone();
            app.manager.shutdown(Box::new(move || {
                if !done.replace(true) {
                    finish();
                }
            }));
        }
        glib::timeout_add_local_once(Duration::from_millis(1500), move || {
            if !done.replace(true) {
                finish();
            }
        });
    });
    outer_application.add_action(&quit);
}

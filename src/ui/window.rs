use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::rc::Rc;
use std::time::Duration;

use adw::prelude::*;
use gtk::{gio, glib};

use crate::audio::{AudioEvent, LevelTarget, Mix, PulseManager};
use crate::config::{Assignment, Config, MAX_CHANNELS};

use super::channel_strip::ChannelStrip;
use super::heading_label;
use super::outputs::OutputsPanel;
use super::sidebar::Sidebar;

struct App {
    config: Rc<RefCell<Config>>,
    manager: PulseManager,
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
    /// Forces every strip and the add-channel card to the same width.
    strip_size_group: gtk::SizeGroup,
}

pub fn build(application: &adw::Application) -> adw::ApplicationWindow {
    let config = Rc::new(RefCell::new(Config::load()));
    let manager = PulseManager::new(config.clone());
    let outputs = OutputsPanel::new();
    let sidebar = Sidebar::new();

    outputs.load_config(&config.borrow().master);

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
    menu.append(Some("About OpenWave"), Some("win.about"));
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

    let toolbar = adw::ToolbarView::new();
    toolbar.add_top_bar(&header);
    toolbar.set_content(Some(&stack));

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
            }
            AudioEvent::Failed(msg) => {
                app.error_page.set_description(Some(&msg));
                app.stack.set_visible_child_name("error");
            }
            AudioEvent::DevicesChanged => refresh_devices(&app),
            AudioEvent::Level(target, v) => match target {
                LevelTarget::Channel(id) => {
                    if let Some((_, strip)) =
                        app.strips.borrow().iter().find(|(cid, _)| *cid == id)
                    {
                        strip.level.set_value(v);
                    }
                }
                LevelTarget::MonitorMix => app.outputs.monitor_level.set_value(v),
                LevelTarget::StreamMix => app.outputs.stream_level.set_value(v),
            },
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
    let mut strips = Vec::with_capacity(channels.len());
    for ch in &channels {
        let strip = ChannelStrip::new();
        strip.load_config(ch);
        if ch.permanent {
            // Keep the button allocated so permanent strips get the exact
            // same header layout as removable ones.
            strip.remove.set_opacity(0.0);
            strip.remove.set_sensitive(false);
            strip.remove.set_tooltip_text(None);
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
        let other = strip.stream_scale.clone();
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
                other.set_value(v);
            }
            schedule_save(&app);
        });
    }

    {
        let app = app.clone();
        let guard = strip.guard.clone();
        let other = strip.monitor_scale.clone();
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
                other.set_value(v);
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
        let monitor_scale = strip.monitor_scale.clone();
        let stream_scale = strip.stream_scale.clone();
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
            }
            schedule_save(&app);
        });
    }

    {
        let app = app.clone();
        strip.remove.connect_clicked(move |_| {
            app.config.borrow_mut().remove_channel(id);
            app.manager.rebuild_channel(id);
            rebuild_strips(&app);
            schedule_save(&app);
            update_sidebar(&app);
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
}

// ---- Window actions -------------------------------------------------------------

fn wire_actions(app: &Rc<App>, window: &adw::ApplicationWindow) {
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

    let about = gio::SimpleAction::new("about", None);
    let win_weak = window.downgrade();
    about.connect_activate(move |_, _| {
        if let Some(win) = win_weak.upgrade() {
            let dialog = adw::AboutDialog::builder()
                .application_name("OpenWave")
                .application_icon("audio-card")
                .developer_name("GhostZero")
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

fn wire_close(app: &Rc<App>, window: &adw::ApplicationWindow) {
    let app = app.clone();
    window.connect_close_request(move |win| {
        app.config.borrow().save();
        let done = Rc::new(Cell::new(false));
        {
            let done = done.clone();
            let win = win.clone();
            app.manager.shutdown(Box::new(move || {
                if !done.replace(true) {
                    win.destroy();
                }
            }));
        }
        {
            let win = win.clone();
            glib::timeout_add_local_once(Duration::from_millis(1500), move || {
                if !done.replace(true) {
                    win.destroy();
                }
            });
        }
        glib::Propagation::Stop
    });
}

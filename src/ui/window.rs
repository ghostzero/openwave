use std::cell::{Cell, RefCell};
use std::rc::Rc;
use std::time::Duration;

use adw::prelude::*;
use gtk::{gio, glib};

use crate::audio::{AudioEvent, LevelTarget, Mix, PulseManager};
use crate::config::{Assignment, Config, CHANNEL_COUNT};

use super::channel_strip::ChannelStrip;
use super::heading_label;
use super::outputs::OutputsPanel;
use super::sidebar::Sidebar;

struct App {
    config: Rc<RefCell<Config>>,
    manager: PulseManager,
    strips: Vec<ChannelStrip>,
    outputs: OutputsPanel,
    sidebar: Sidebar,
    stack: gtk::Stack,
    error_page: adw::StatusPage,
    save_pending: Cell<bool>,
    /// Per-channel edit counter used to debounce rename → sink rebuild.
    rename_epoch: RefCell<Vec<u64>>,
}

pub fn build(application: &adw::Application) -> adw::ApplicationWindow {
    let config = Rc::new(RefCell::new(Config::load()));
    let manager = PulseManager::new(config.clone());
    let strips: Vec<ChannelStrip> = (0..CHANNEL_COUNT).map(|_| ChannelStrip::new()).collect();
    let outputs = OutputsPanel::new();
    let sidebar = Sidebar::new();

    {
        let cfg = config.borrow();
        for (i, strip) in strips.iter().enumerate() {
            strip.load_config(&cfg.channels[i]);
        }
        outputs.load_config(&cfg.master);
    }

    // ---- Mixer page ----------------------------------------------------------
    let strips_box = gtk::Box::new(gtk::Orientation::Horizontal, 12);
    strips_box.set_homogeneous(true);
    for strip in &strips {
        strips_box.append(&strip.root);
    }
    let strips_scroller = gtk::ScrolledWindow::builder()
        .hscrollbar_policy(gtk::PolicyType::Automatic)
        .vscrollbar_policy(gtk::PolicyType::Never)
        .child(&strips_box)
        .vexpand(true)
        .build();

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
        strips,
        outputs,
        sidebar,
        stack,
        error_page,
        save_pending: Cell::new(false),
        rename_epoch: RefCell::new(vec![0; CHANNEL_COUNT]),
    });

    wire_actions(&app, &window);
    wire_strips(&app);
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
                LevelTarget::Channel(i) => app.strips[i].level.set_value(v),
                LevelTarget::MonitorMix => app.outputs.monitor_level.set_value(v),
                LevelTarget::StreamMix => app.outputs.stream_level.set_value(v),
            },
        }
    });
}

fn refresh_devices(app: &Rc<App>) {
    let sources = app.manager.sources();
    let apps = app.manager.app_names();

    let mut items: Vec<(String, Option<Assignment>)> = vec![("No Input".to_string(), None)];
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
        for (i, strip) in app.strips.iter().enumerate() {
            strip.set_input_entries(&items, &cfg.channels[i].assignment);
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
    });
}

// ---- User interaction wiring -------------------------------------------------

fn wire_strips(app: &Rc<App>) {
    for i in 0..CHANNEL_COUNT {
        let strip = &app.strips[i];

        {
            let app = app.clone();
            strip.name.connect_changed(move |editable| {
                if app.strips[i].guard.get() {
                    return;
                }
                app.config.borrow_mut().channels[i].name = editable.text().to_string();
                schedule_save(&app);
                update_sidebar(&app);
                // App channels expose a device named after the channel;
                // rebuild the channel sink once the user stops typing.
                let epoch = {
                    let mut epochs = app.rename_epoch.borrow_mut();
                    epochs[i] += 1;
                    epochs[i]
                };
                let app = app.clone();
                glib::timeout_add_local_once(Duration::from_millis(900), move || {
                    if app.rename_epoch.borrow()[i] != epoch {
                        return;
                    }
                    let is_app_channel = matches!(
                        app.config.borrow().channels[i].assignment,
                        Some(Assignment::App { .. })
                    );
                    if is_app_channel {
                        app.manager.rebuild_channel(i);
                    }
                });
            });
        }

        {
            let app = app.clone();
            strip.input.connect_selected_notify(move |dd| {
                if app.strips[i].guard.get() {
                    return;
                }
                let assignment = app.strips[i]
                    .entries
                    .borrow()
                    .get(dd.selected() as usize)
                    .cloned()
                    .flatten();
                {
                    let mut cfg = app.config.borrow_mut();
                    if cfg.channels[i].assignment == assignment {
                        return;
                    }
                    cfg.channels[i].assignment = assignment;
                }
                app.manager.rebuild_channel(i);
                schedule_save(&app);
                update_sidebar(&app);
            });
        }

        {
            let app = app.clone();
            strip.monitor_scale.connect_value_changed(move |scale| {
                if app.strips[i].guard.get() {
                    return;
                }
                let v = scale.value();
                let linked = {
                    let mut cfg = app.config.borrow_mut();
                    cfg.channels[i].monitor_volume = v;
                    cfg.channels[i].linked
                };
                app.manager.apply_channel_mix(i, Mix::Monitor);
                if linked {
                    app.strips[i].stream_scale.set_value(v);
                }
                schedule_save(&app);
            });
        }

        {
            let app = app.clone();
            strip.stream_scale.connect_value_changed(move |scale| {
                if app.strips[i].guard.get() {
                    return;
                }
                let v = scale.value();
                let linked = {
                    let mut cfg = app.config.borrow_mut();
                    cfg.channels[i].stream_volume = v;
                    cfg.channels[i].linked
                };
                app.manager.apply_channel_mix(i, Mix::Stream);
                if linked {
                    app.strips[i].monitor_scale.set_value(v);
                }
                schedule_save(&app);
            });
        }

        {
            let app = app.clone();
            strip.monitor_mute.connect_toggled(move |btn| {
                if app.strips[i].guard.get() {
                    return;
                }
                app.config.borrow_mut().channels[i].monitor_muted = btn.is_active();
                app.manager.apply_channel_mix(i, Mix::Monitor);
                schedule_save(&app);
            });
        }

        {
            let app = app.clone();
            strip.stream_mute.connect_toggled(move |btn| {
                if app.strips[i].guard.get() {
                    return;
                }
                app.config.borrow_mut().channels[i].stream_muted = btn.is_active();
                app.manager.apply_channel_mix(i, Mix::Stream);
                schedule_save(&app);
            });
        }

        {
            let app = app.clone();
            strip.link.connect_toggled(move |btn| {
                if app.strips[i].guard.get() {
                    return;
                }
                app.config.borrow_mut().channels[i].linked = btn.is_active();
                if btn.is_active() {
                    let v = app.strips[i].monitor_scale.value();
                    app.strips[i].stream_scale.set_value(v);
                }
                schedule_save(&app);
            });
        }
    }
}

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

fn wire_actions(app: &Rc<App>, window: &adw::ApplicationWindow) {
    let _ = app;
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

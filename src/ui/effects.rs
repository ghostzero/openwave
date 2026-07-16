//! Per-channel effects dialog: an ordered LV2 plugin chain with inline
//! parameter editing, plus the optional Carla VST rack.

use std::cell::RefCell;
use std::rc::Rc;
use std::time::Duration;

use adw::prelude::*;
use gtk::glib;

use crate::audio::PulseManager;
use crate::config::Config;
use crate::lv2;

/// Everything the dialog needs from the main window.
pub struct EffectsDeps {
    pub config: Rc<RefCell<Config>>,
    pub manager: PulseManager,
    /// Called after add/remove/reorder/enable/rack changes: persist config,
    /// rebuild the channel routing and refresh the main UI.
    pub on_structure: Rc<dyn Fn(u64)>,
    /// Called after a live control-value change: persist config only.
    pub on_control: Rc<dyn Fn(u64)>,
}

struct DialogState {
    channel_id: u64,
    deps: EffectsDeps,
    /// Container the LV2 group is rebuilt into.
    effects_box: gtk::Box,
    dialog: adw::Dialog,
    /// Trailing-edge debouncer for pw-cli control updates while a slider
    /// is being dragged.
    control_debounce: RefCell<Option<glib::SourceId>>,
}

pub fn open(parent: &impl IsA<gtk::Widget>, deps: EffectsDeps, channel_id: u64) {
    let channel_name = deps
        .config
        .borrow()
        .channel(channel_id)
        .map(|c| c.name.clone())
        .unwrap_or_default();

    let content = gtk::Box::new(gtk::Orientation::Vertical, 24);
    content.set_margin_top(12);
    content.set_margin_bottom(24);
    content.set_margin_start(16);
    content.set_margin_end(16);

    let effects_box = gtk::Box::new(gtk::Orientation::Vertical, 0);
    content.append(&effects_box);

    let dialog = adw::Dialog::builder()
        .title(format!("Effects — {channel_name}"))
        .content_width(560)
        .content_height(640)
        .build();

    let state = Rc::new(DialogState {
        channel_id,
        deps,
        effects_box,
        dialog: dialog.clone(),
        control_debounce: RefCell::new(None),
    });

    rebuild_effects_group(&state);
    content.append(&build_vst_group(&state));

    let scroller = gtk::ScrolledWindow::builder()
        .hscrollbar_policy(gtk::PolicyType::Never)
        .child(&adw::Clamp::builder().maximum_size(520).child(&content).build())
        .vexpand(true)
        .build();

    let toolbar = adw::ToolbarView::new();
    toolbar.add_top_bar(&adw::HeaderBar::new());
    toolbar.set_content(Some(&scroller));
    dialog.set_child(Some(&toolbar));
    dialog.present(Some(parent));
}

// ---- LV2 chain -------------------------------------------------------------

fn rebuild_effects_group(state: &Rc<DialogState>) {
    while let Some(child) = state.effects_box.first_child() {
        state.effects_box.remove(&child);
    }

    let group = adw::PreferencesGroup::builder()
        .title("Effect Chain (LV2)")
        .description("Applied to this input before it reaches the monitor and stream mixes, top to bottom.")
        .build();

    let effects = state
        .deps
        .config
        .borrow()
        .channel(state.channel_id)
        .map(|c| c.effects.clone())
        .unwrap_or_default();
    let catalog = lv2::catalog();
    let count = effects.len();

    for (pos, effect) in effects.iter().enumerate() {
        let row = adw::ExpanderRow::builder()
            .title(glib::markup_escape_text(&effect.name))
            .build();

        let enable = gtk::Switch::builder()
            .active(effect.enabled)
            .valign(gtk::Align::Center)
            .tooltip_text("Enable this effect")
            .build();
        {
            let state = state.clone();
            let effect_id = effect.id;
            enable.connect_state_set(move |_, on| {
                structural_change(&state, |ch| {
                    if let Some(e) = ch.effect_mut(effect_id) {
                        e.enabled = on;
                    }
                });
                glib::Propagation::Proceed
            });
        }
        row.add_prefix(&enable);

        let controls_box = gtk::Box::new(gtk::Orientation::Horizontal, 0);
        controls_box.set_valign(gtk::Align::Center);
        for (icon, tip, delta) in [
            ("go-up-symbolic", "Move up", -1i32),
            ("go-down-symbolic", "Move down", 1),
        ] {
            let btn = gtk::Button::builder()
                .icon_name(icon)
                .tooltip_text(tip)
                .valign(gtk::Align::Center)
                .sensitive(if delta < 0 { pos > 0 } else { pos + 1 < count })
                .build();
            btn.add_css_class("flat");
            let state = state.clone();
            let effect_id = effect.id;
            btn.connect_clicked(move |_| {
                structural_change(&state, |ch| {
                    if let Some(i) = ch.effects.iter().position(|e| e.id == effect_id) {
                        let j = i as i32 + delta;
                        if j >= 0 && (j as usize) < ch.effects.len() {
                            ch.effects.swap(i, j as usize);
                        }
                    }
                });
            });
            controls_box.append(&btn);
        }
        let remove = gtk::Button::builder()
            .icon_name("user-trash-symbolic")
            .tooltip_text("Remove this effect")
            .valign(gtk::Align::Center)
            .build();
        remove.add_css_class("flat");
        {
            let state = state.clone();
            let effect_id = effect.id;
            remove.connect_clicked(move |_| {
                structural_change(&state, |ch| {
                    ch.effects.retain(|e| e.id != effect_id);
                });
            });
        }
        controls_box.append(&remove);
        row.add_suffix(&controls_box);

        match catalog.as_ref().and_then(|c| c.find(&effect.uri)) {
            Some(info) => {
                row.set_subtitle(&glib::markup_escape_text(&effect.uri));
                let mono = info.is_mono();
                for port in &info.controls {
                    let value = effect
                        .controls
                        .get(&port.symbol)
                        .copied()
                        .unwrap_or(f64::from(port.default));
                    row.add_row(&control_row(
                        state, effect.id, mono, port, value,
                    ));
                }
                if info.controls.is_empty() {
                    row.set_enable_expansion(false);
                }
            }
            None => {
                row.set_subtitle("Plugin is not installed — the effect is skipped");
                row.set_enable_expansion(false);
            }
        }
        group.add(&row);
    }

    // Add-effect entry point (or hints when nothing can be added).
    if lv2::available() {
        let add_row = adw::ButtonRow::builder()
            .title("Add Effect…")
            .start_icon_name("list-add-symbolic")
            .build();
        let state_c = state.clone();
        add_row.connect_activated(move |_| open_plugin_picker(&state_c));
        group.add(&add_row);
        if count == 0
            && catalog.as_ref().is_none_or(|c| c.plugins.is_empty())
        {
            group.set_description(Some(
                "No LV2 plugins found. Install some (e.g. the LSP plugin \
                 collection: lsp-plugins-lv2) to add effects here.",
            ));
        }
    } else {
        group.set_description(Some(
            "LV2 support requires the lilv library, which could not be \
             loaded. Install lilv to add effects here.",
        ));
    }

    state.effects_box.append(&group);
}

/// One parameter row: a switch for toggle ports, otherwise a slider.
fn control_row(
    state: &Rc<DialogState>,
    effect_id: u64,
    mono: bool,
    port: &lv2::ControlPort,
    value: f64,
) -> adw::ActionRow {
    let row = adw::ActionRow::builder()
        .title(glib::markup_escape_text(&port.name))
        .build();
    let symbol = port.symbol.clone();
    if port.toggled {
        let sw = gtk::Switch::builder()
            .active(value > 0.5)
            .valign(gtk::Align::Center)
            .build();
        let state = state.clone();
        sw.connect_state_set(move |_, on| {
            control_change(&state, effect_id, mono, &symbol, if on { 1.0 } else { 0.0 });
            glib::Propagation::Proceed
        });
        row.add_suffix(&sw);
        row.set_activatable_widget(Some(&sw));
    } else {
        let min = f64::from(port.min);
        let max = f64::from(port.max);
        let step = if port.integer {
            1.0
        } else {
            (max - min) / 200.0
        };
        let scale = gtk::Scale::with_range(gtk::Orientation::Horizontal, min, max, step);
        scale.set_value(value.clamp(min, max));
        scale.set_width_request(200);
        scale.set_draw_value(true);
        scale.set_value_pos(gtk::PositionType::Right);
        scale.set_digits(if port.integer { 0 } else { 2 });
        scale.set_valign(gtk::Align::Center);
        let state = state.clone();
        scale.connect_value_changed(move |s| {
            control_change(&state, effect_id, mono, &symbol, s.value());
        });
        row.add_suffix(&scale);
    }
    row
}

/// Persist a control value and (debounced) push it into the running chain.
fn control_change(
    state: &Rc<DialogState>,
    effect_id: u64,
    mono: bool,
    symbol: &str,
    value: f64,
) {
    {
        let mut cfg = state.deps.config.borrow_mut();
        if let Some(ch) = cfg.channel_mut(state.channel_id) {
            if let Some(e) = ch.effect_mut(effect_id) {
                e.controls.insert(symbol.to_string(), value);
            }
        }
    }
    (state.deps.on_control)(state.channel_id);

    if let Some(old) = state.control_debounce.borrow_mut().take() {
        old.remove();
    }
    let state_c = state.clone();
    let symbol = symbol.to_string();
    let source = glib::timeout_add_local_once(Duration::from_millis(80), move || {
        *state_c.control_debounce.borrow_mut() = None;
        state_c.deps.manager.set_effect_control(
            state_c.channel_id,
            effect_id,
            mono,
            &symbol,
            value,
        );
    });
    *state.control_debounce.borrow_mut() = Some(source);
}

/// Apply `f` to the channel config, then rebuild routing and this dialog.
fn structural_change(
    state: &Rc<DialogState>,
    f: impl FnOnce(&mut crate::config::ChannelConfig),
) {
    {
        let mut cfg = state.deps.config.borrow_mut();
        let Some(ch) = cfg.channel_mut(state.channel_id) else {
            return;
        };
        f(ch);
    }
    (state.deps.on_structure)(state.channel_id);
    // Deferred: the change may originate from a widget this rebuild removes.
    let state = state.clone();
    glib::idle_add_local_once(move || rebuild_effects_group(&state));
}

// ---- Plugin picker -----------------------------------------------------------

fn open_plugin_picker(state: &Rc<DialogState>) {
    let Some(catalog) = lv2::catalog() else {
        return;
    };

    let search = gtk::SearchEntry::builder()
        .placeholder_text("Search plugins…")
        .hexpand(true)
        .build();

    let list = gtk::ListBox::builder()
        .selection_mode(gtk::SelectionMode::None)
        .css_classes(["boxed-list"])
        .build();

    let picker = adw::Dialog::builder()
        .title("Add Effect")
        .content_width(480)
        .content_height(600)
        .build();

    for plugin in &catalog.plugins {
        let row = adw::ActionRow::builder()
            .title(glib::markup_escape_text(&plugin.name))
            .subtitle(glib::markup_escape_text(&plugin.uri))
            .activatable(true)
            .build();
        row.add_suffix(&gtk::Image::from_icon_name("list-add-symbolic"));
        let state = state.clone();
        let picker = picker.clone();
        let uri = plugin.uri.clone();
        let name = plugin.name.clone();
        row.connect_activated(move |_| {
            structural_change(&state, |ch| {
                ch.add_effect(&uri, &name);
            });
            picker.close();
        });
        list.append(&row);
    }

    {
        let search = search.clone();
        list.set_filter_func(move |row| {
            let text = search.text().to_lowercase();
            if text.is_empty() {
                return true;
            }
            let Some(row) = row.downcast_ref::<adw::ActionRow>() else {
                return true;
            };
            row.title().to_lowercase().contains(&text)
                || row.subtitle().is_some_and(|s| s.to_lowercase().contains(&text))
        });
    }
    {
        let list = list.clone();
        search.connect_search_changed(move |_| list.invalidate_filter());
    }

    let content = gtk::Box::new(gtk::Orientation::Vertical, 12);
    content.set_margin_top(12);
    content.set_margin_bottom(16);
    content.set_margin_start(16);
    content.set_margin_end(16);
    content.append(&search);
    content.append(&list);

    let scroller = gtk::ScrolledWindow::builder()
        .hscrollbar_policy(gtk::PolicyType::Never)
        .child(&content)
        .vexpand(true)
        .build();
    let toolbar = adw::ToolbarView::new();
    toolbar.add_top_bar(&adw::HeaderBar::new());
    toolbar.set_content(Some(&scroller));
    picker.set_child(Some(&toolbar));
    picker.present(Some(&state.dialog));
}

// ---- Carla / VST rack ---------------------------------------------------------

fn build_vst_group(state: &Rc<DialogState>) -> adw::PreferencesGroup {
    let group = adw::PreferencesGroup::builder()
        .title("VST Rack (Carla)")
        .build();
    let available = state.deps.manager.carla_available();

    let enable = adw::SwitchRow::builder()
        .title("Enable VST Rack")
        .subtitle("Route this input through a Carla rack hosting VST2/VST3 plugins, in front of the LV2 chain")
        .sensitive(available)
        .build();
    let open_row = adw::ActionRow::builder()
        .title("Plugin Rack")
        .subtitle("Add and edit VST plugins in the Carla window. Save there (Ctrl+S) so the rack is restored next time.")
        .build();
    let open_btn = gtk::Button::builder()
        .label("Open")
        .valign(gtk::Align::Center)
        .build();
    open_row.add_suffix(&open_btn);

    if !available {
        group.set_description(Some(
            "Carla is not installed. Install the “Carla” package to host \
             VST plugins on this channel.",
        ));
    }

    let rack_on = state
        .deps
        .config
        .borrow()
        .channel(state.channel_id)
        .map(|c| c.vst_rack)
        .unwrap_or(false);
    enable.set_active(rack_on && available);
    open_row.set_sensitive(available && rack_on);

    {
        let state = state.clone();
        let open_row = open_row.clone();
        enable.connect_active_notify(move |sw| {
            let on = sw.is_active();
            {
                let mut cfg = state.deps.config.borrow_mut();
                let Some(ch) = cfg.channel_mut(state.channel_id) else {
                    return;
                };
                if ch.vst_rack == on {
                    return;
                }
                ch.vst_rack = on;
            }
            open_row.set_sensitive(on);
            (state.deps.on_structure)(state.channel_id);
        });
    }
    {
        let state = state.clone();
        open_btn.connect_clicked(move |_| {
            state.deps.manager.open_carla(state.channel_id);
        });
    }

    group.add(&enable);
    group.add(&open_row);
    group
}

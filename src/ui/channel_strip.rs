use std::cell::{Cell, RefCell};
use std::rc::Rc;

use adw::prelude::*;

use crate::config::{Assignment, ChannelConfig};

use super::{label_factory, meter_pair, mute_button, MeterPair};

/// Fixed width of every channel strip (and the add-channel card).
pub const STRIP_WIDTH: i32 = 150;

/// One vertical input strip: rename label, input selector, level meter and the
/// two independent faders (monitor mix left, stream mix right) with per-mix
/// mute buttons and an optional fader link.
pub struct ChannelStrip {
    pub root: gtk::Box,
    pub name: gtk::EditableLabel,
    pub remove: gtk::Button,
    pub fx: gtk::Button,
    pub input: gtk::DropDown,
    pub noise: gtk::Switch,
    noise_row: gtk::Box,
    level: MeterPair,
    pub monitor_scale: gtk::Scale,
    pub stream_scale: gtk::Scale,
    pub monitor_mute: gtk::ToggleButton,
    pub stream_mute: gtk::ToggleButton,
    pub link: gtk::ToggleButton,
    /// Set while the strip is being updated programmatically so signal
    /// handlers know not to write back into the config.
    pub guard: Rc<Cell<bool>>,
    /// Assignment behind each drop-down position (index-aligned with the
    /// model). Shared with the selection handler.
    pub entries: Rc<RefCell<Vec<Option<Assignment>>>>,
    last_labels: RefCell<Vec<String>>,
}

fn fader() -> gtk::Scale {
    let scale = gtk::Scale::with_range(gtk::Orientation::Vertical, 0.0, 1.0, 0.01);
    scale.set_inverted(true);
    scale.set_draw_value(false);
    scale.set_vexpand(true);
    scale.set_halign(gtk::Align::Center);
    scale.add_css_class("fader");
    scale
}

fn fader_column(scale: &gtk::Scale, mute: &gtk::ToggleButton, caption: &str) -> gtk::Box {
    let col = gtk::Box::new(gtk::Orientation::Vertical, 6);
    col.set_hexpand(true);
    col.append(scale);
    col.append(mute);
    let label = gtk::Label::new(Some(caption));
    label.add_css_class("caption");
    label.add_css_class("dim-label");
    col.append(&label);
    col
}

impl ChannelStrip {
    pub fn new() -> Self {
        let root = gtk::Box::builder()
            .orientation(gtk::Orientation::Vertical)
            .spacing(10)
            .width_request(STRIP_WIDTH)
            .css_classes(["card", "channel-strip"])
            .build();

        let name = gtk::EditableLabel::builder()
            .text("")
            .xalign(0.0)
            .hexpand(true)
            .max_width_chars(12)
            .tooltip_text("Rename channel")
            .build();
        name.add_css_class("heading");

        let remove = gtk::Button::builder()
            .icon_name("user-trash-symbolic")
            .tooltip_text("Remove channel")
            .valign(gtk::Align::Center)
            .build();
        remove.add_css_class("flat");
        remove.add_css_class("circular");

        let fx = gtk::Button::builder()
            .icon_name("sound-wave-symbolic")
            .tooltip_text("Effects")
            .valign(gtk::Align::Center)
            .build();
        fx.add_css_class("flat");
        fx.add_css_class("circular");

        let header = gtk::Box::new(gtk::Orientation::Horizontal, 4);
        header.append(&name);
        header.append(&fx);
        header.append(&remove);

        let input = gtk::DropDown::builder()
            .tooltip_text("Select the input for this channel")
            .build();
        input.set_factory(Some(&label_factory(9, true)));
        input.set_list_factory(Some(&label_factory(36, false)));

        let noise = gtk::Switch::builder()
            .valign(gtk::Align::Center)
            .build();
        let noise_label = gtk::Label::builder()
            .label("Noise Removal")
            .xalign(0.0)
            .hexpand(true)
            .ellipsize(gtk::pango::EllipsizeMode::End)
            .css_classes(["caption"])
            .build();
        let noise_row = gtk::Box::builder()
            .orientation(gtk::Orientation::Horizontal)
            .spacing(6)
            .tooltip_text("Suppress background noise on this input (RNNoise)")
            .visible(false)
            .build();
        noise_row.append(&noise_label);
        noise_row.append(&noise);

        let level = meter_pair();

        let monitor_scale = fader();
        monitor_scale.set_tooltip_text(Some("Monitor mix volume"));
        let stream_scale = fader();
        stream_scale.set_tooltip_text(Some("Stream mix volume"));

        let monitor_mute = mute_button("audio-headphones-symbolic", "Mute in the monitor mix");
        let stream_mute = mute_button("media-record-symbolic", "Mute in the stream mix");

        let link = gtk::ToggleButton::builder()
            .icon_name("insert-link-symbolic")
            .tooltip_text("Link both faders")
            .valign(gtk::Align::Center)
            .build();
        link.add_css_class("flat");
        link.add_css_class("circular");

        let faders = gtk::Box::new(gtk::Orientation::Horizontal, 2);
        faders.set_vexpand(true);
        faders.append(&fader_column(&monitor_scale, &monitor_mute, "Monitor"));
        faders.append(&link);
        faders.append(&fader_column(&stream_scale, &stream_mute, "Stream"));

        root.append(&header);
        root.append(&input);
        root.append(&noise_row);
        root.append(&level.root);
        root.append(&faders);

        Self {
            root,
            name,
            remove,
            fx,
            input,
            noise,
            noise_row,
            level,
            monitor_scale,
            stream_scale,
            monitor_mute,
            stream_mute,
            link,
            guard: Rc::new(Cell::new(false)),
            entries: Rc::new(RefCell::new(Vec::new())),
            last_labels: RefCell::new(Vec::new()),
        }
    }

    /// Feed measured peaks: one value for mono inputs, two for stereo. The
    /// right meter is shown only while the input reports stereo levels.
    pub fn set_levels(&self, values: &[f64]) {
        self.level.set_levels(values);
    }

    /// Push the current config values into the widgets without firing the
    /// user-edit handlers.
    pub fn load_config(&self, c: &ChannelConfig) {
        self.guard.set(true);
        self.name.set_text(&c.name);
        self.monitor_scale.set_value(c.monitor_volume);
        self.stream_scale.set_value(c.stream_volume);
        self.monitor_mute.set_active(c.monitor_muted);
        self.stream_mute.set_active(c.stream_muted);
        self.link.set_active(c.linked);
        self.update_fx_indicator(c);
        self.update_noise_toggle(c);
        self.guard.set(false);
    }

    /// Show the noise-suppression switch on capture-source channels and sync
    /// it with the presence of an enabled RNNoise effect in the chain.
    pub fn update_noise_toggle(&self, c: &ChannelConfig) {
        let was = self.guard.replace(true);
        self.noise_row
            .set_visible(matches!(c.assignment, Some(Assignment::Source { .. })));
        self.noise.set_active(c.noise_suppression_active());
        self.guard.set(was);
    }

    /// Tint the FX button while the channel processes through effects.
    pub fn update_fx_indicator(&self, c: &ChannelConfig) {
        let lv2 = c.effects.iter().filter(|e| e.enabled).count();
        let vst = c.vst_plugins.iter().filter(|p| p.enabled).count();
        if c.fx_active() {
            self.fx.add_css_class("accent");
        } else {
            self.fx.remove_css_class("accent");
        }
        let mut tip = String::from("Effects");
        let mut parts = Vec::new();
        if vst > 0 {
            parts.push(format!("{vst} VST"));
        }
        if lv2 > 0 {
            parts.push(format!("{lv2} LV2"));
        }
        if !parts.is_empty() {
            tip = format!("Effects ({} active)", parts.join(" + "));
        }
        self.fx.set_tooltip_text(Some(&tip));
    }

    /// Rebuild the input drop-down. `current` is kept selected; if it is not
    /// in `items` (device unplugged, app not running) a placeholder entry is
    /// appended so the assignment is not silently lost.
    pub fn set_input_entries(
        &self,
        items: &[(String, Option<Assignment>)],
        current: &Option<Assignment>,
    ) {
        let mut labels: Vec<String> = items.iter().map(|(l, _)| l.clone()).collect();
        let mut assigns: Vec<Option<Assignment>> = items.iter().map(|(_, a)| a.clone()).collect();
        let mut selected = assigns.iter().position(|a| a == current);
        if selected.is_none()
            && let Some(a) = current {
                let label = match a {
                    Assignment::Source { name } => format!("{name} (unavailable)"),
                    Assignment::App { name } => format!("{name} (not running)"),
                    Assignment::Virtual => "Virtual Device".to_string(),
                };
                labels.push(label);
                assigns.push(Some(a.clone()));
                selected = Some(labels.len() - 1);
            }
        let selected = selected.unwrap_or(0) as u32;
        if *self.last_labels.borrow() == labels && self.input.selected() == selected {
            return;
        }
        self.guard.set(true);
        let strs: Vec<&str> = labels.iter().map(String::as_str).collect();
        let model = gtk::StringList::new(&strs);
        self.input.set_model(Some(&model));
        self.input.set_selected(selected);
        self.guard.set(false);
        *self.last_labels.borrow_mut() = labels;
        *self.entries.borrow_mut() = assigns;
    }
}

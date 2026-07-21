use std::cell::{Cell, RefCell};
use std::rc::Rc;

use adw::prelude::*;

use crate::config::{Assignment, ChannelConfig};

use super::{label_factory, meter_pair, mute_button, MeterPair};

/// Fixed width of every channel strip (and the add-channel card).
pub const STRIP_WIDTH: i32 = 150;
/// Wider strip while the third (VOD) fader is shown.
const STRIP_WIDTH_VOD: i32 = 200;

/// One vertical input strip: rename label, input selector, level meter and
/// independent per-mix faders (monitor, stream, and — when the VOD mix is
/// enabled — VOD) with per-mix mute buttons and a single link toggle that
/// ties all faders together.
pub struct ChannelStrip {
    pub root: gtk::Box,
    pub name: gtk::EditableLabel,
    pub remove: gtk::Button,
    pub fx: gtk::Button,
    pub input: gtk::DropDown,
    level: MeterPair,
    pub monitor_scale: gtk::Scale,
    pub stream_scale: gtk::Scale,
    pub vod_scale: gtk::Scale,
    pub monitor_mute: gtk::ToggleButton,
    pub stream_mute: gtk::ToggleButton,
    pub vod_mute: gtk::ToggleButton,
    pub link: gtk::ToggleButton,
    vod_caption: gtk::Label,
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

/// One row of the fader block. Homogeneous, so every row splits the strip
/// into equal columns and the scales, mute buttons and captions line up —
/// also when the VOD column is hidden and the rows fall back to two columns.
fn mix_row() -> gtk::Box {
    let row = gtk::Box::new(gtk::Orientation::Horizontal, 2);
    row.set_homogeneous(true);
    row
}

fn caption_label(text: &str) -> gtk::Label {
    let label = gtk::Label::new(Some(text));
    label.add_css_class("caption");
    label.add_css_class("dim-label");
    label
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

        let level = meter_pair();

        let monitor_scale = fader();
        monitor_scale.set_tooltip_text(Some("Monitor mix volume"));
        let stream_scale = fader();
        stream_scale.set_tooltip_text(Some("Stream mix volume"));
        let vod_scale = fader();
        vod_scale.set_tooltip_text(Some("VOD mix volume"));

        let monitor_mute = mute_button("audio-headphones-symbolic", "Mute in the monitor mix");
        let stream_mute = mute_button("media-record-symbolic", "Mute in the stream mix");
        let vod_mute = mute_button("camera-video-symbolic", "Mute in the VOD mix");

        let link = gtk::ToggleButton::builder()
            .icon_name("insert-link-symbolic")
            .tooltip_text("Link the faders")
            .halign(gtk::Align::Center)
            .build();
        link.add_css_class("flat");
        link.add_css_class("circular");

        // Fader block: a row of scales, the link toggle, a row of mute
        // buttons and a row of captions, in equal columns so each fader's
        // controls stack up exactly below it.
        let scales_row = mix_row();
        scales_row.set_vexpand(true);
        scales_row.append(&monitor_scale);
        scales_row.append(&stream_scale);
        scales_row.append(&vod_scale);

        let mutes_row = mix_row();
        mutes_row.append(&monitor_mute);
        mutes_row.append(&stream_mute);
        mutes_row.append(&vod_mute);

        let captions_row = mix_row();
        let vod_caption = caption_label("VOD");
        captions_row.append(&caption_label("Monitor"));
        captions_row.append(&caption_label("Stream"));
        captions_row.append(&vod_caption);

        vod_scale.set_visible(false);
        vod_mute.set_visible(false);
        vod_caption.set_visible(false);

        let faders = gtk::Box::new(gtk::Orientation::Vertical, 6);
        faders.set_vexpand(true);
        faders.append(&scales_row);
        faders.append(&link);
        faders.append(&mutes_row);
        faders.append(&captions_row);

        root.append(&header);
        root.append(&input);
        root.append(&level.root);
        root.append(&faders);

        Self {
            root,
            name,
            remove,
            fx,
            input,
            level,
            monitor_scale,
            stream_scale,
            vod_scale,
            monitor_mute,
            stream_mute,
            vod_mute,
            link,
            vod_caption,
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
        self.vod_scale.set_value(c.vod_volume);
        self.monitor_mute.set_active(c.monitor_muted);
        self.stream_mute.set_active(c.stream_muted);
        self.vod_mute.set_active(c.vod_muted);
        self.link.set_active(c.linked);
        self.update_fx_indicator(c);
        self.guard.set(false);
    }

    /// Show or hide the VOD fader column (and widen the strip to fit it).
    pub fn set_vod_visible(&self, visible: bool) {
        self.vod_scale.set_visible(visible);
        self.vod_mute.set_visible(visible);
        self.vod_caption.set_visible(visible);
        self.root.set_width_request(if visible {
            STRIP_WIDTH_VOD
        } else {
            STRIP_WIDTH
        });
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

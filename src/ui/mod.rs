pub mod channel_strip;
pub mod effects;
pub mod outputs;
pub mod sidebar;
pub mod window;

use adw::prelude::*;
use gtk::{gdk, pango};

const CSS: &str = r#"
.channel-strip {
    padding: 12px;
    min-width: 128px;
}
.channel-strip scale.fader {
    min-height: 200px;
}
levelbar.meter block {
    min-height: 5px;
    border-radius: 3px;
}
levelbar.meter block.filled.meter-ok {
    background-color: #33d17a;
}
levelbar.meter block.filled.meter-warn {
    background-color: #f5c211;
}
levelbar.meter block.filled.meter-clip {
    background-color: #e01b24;
}
.mute-toggle:checked {
    color: #e01b24;
    background-color: alpha(#e01b24, 0.12);
}
"#;

pub fn load_css() {
    let provider = gtk::CssProvider::new();
    provider.load_from_string(CSS);
    if let Some(display) = gdk::Display::default() {
        gtk::style_context_add_provider_for_display(
            &display,
            &provider,
            gtk::STYLE_PROVIDER_PRIORITY_APPLICATION,
        );
    }
}

/// A horizontal audio level meter styled green → yellow → red.
pub fn meter_bar() -> gtk::LevelBar {
    let bar = gtk::LevelBar::builder()
        .min_value(0.0)
        .max_value(1.0)
        .mode(gtk::LevelBarMode::Continuous)
        .valign(gtk::Align::Center)
        .build();
    bar.add_css_class("meter");
    for name in ["low", "high", "full"] {
        bar.remove_offset_value(Some(name));
    }
    bar.add_offset_value("meter-ok", 0.70);
    bar.add_offset_value("meter-warn", 0.90);
    bar.add_offset_value("meter-clip", 1.0);
    bar
}

/// List-item factory rendering plain strings with end-ellipsizing, for use in
/// drop-downs whose entries can be long device names. With `fixed` the label
/// always requests exactly `chars` characters, so the drop-down (and its
/// container) keeps the same width regardless of the selected item.
pub fn label_factory(chars: i32, fixed: bool) -> gtk::SignalListItemFactory {
    let factory = gtk::SignalListItemFactory::new();
    factory.connect_setup(move |_, obj| {
        let Some(item) = obj.downcast_ref::<gtk::ListItem>() else {
            return;
        };
        let label = gtk::Label::builder()
            .xalign(0.0)
            .ellipsize(pango::EllipsizeMode::End)
            .max_width_chars(chars)
            .build();
        if fixed {
            label.set_width_chars(chars);
        }
        item.set_child(Some(&label));
    });
    factory.connect_bind(|_, obj| {
        let Some(item) = obj.downcast_ref::<gtk::ListItem>() else {
            return;
        };
        let Some(label) = item.child().and_downcast::<gtk::Label>() else {
            return;
        };
        let text = item
            .item()
            .and_downcast::<gtk::StringObject>()
            .map(|s| s.string().to_string())
            .unwrap_or_default();
        label.set_text(&text);
        label.set_tooltip_text(Some(&text));
    });
    factory
}

pub fn heading_label(text: &str) -> gtk::Label {
    let label = gtk::Label::builder().label(text).xalign(0.0).build();
    label.add_css_class("heading");
    label
}

pub fn mute_button(icon: &str, tooltip: &str) -> gtk::ToggleButton {
    let btn = gtk::ToggleButton::builder()
        .icon_name(icon)
        .tooltip_text(tooltip)
        .halign(gtk::Align::Center)
        .valign(gtk::Align::Center)
        .build();
    btn.add_css_class("flat");
    btn.add_css_class("circular");
    btn.add_css_class("mute-toggle");
    btn
}

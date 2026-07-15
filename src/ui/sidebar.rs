use adw::prelude::*;
use gtk::glib;

use crate::config::{Assignment, Config, CHANNEL_COUNT};

/// Sidebar summarizing the virtual devices and what feeds each channel.
pub struct Sidebar {
    pub root: gtk::ScrolledWindow,
    monitor_row: adw::ActionRow,
    stream_row: adw::ActionRow,
    channel_rows: Vec<adw::ActionRow>,
}

impl Sidebar {
    pub fn new() -> Self {
        let outputs_group = adw::PreferencesGroup::builder()
            .title("Virtual Outputs")
            .build();

        let monitor_row = adw::ActionRow::builder()
            .title("Monitor Mix")
            .subtitle("Not routed")
            .build();
        monitor_row.add_prefix(&gtk::Image::from_icon_name("audio-headphones-symbolic"));
        outputs_group.add(&monitor_row);

        let stream_row = adw::ActionRow::builder()
            .title("Stream Mix")
            .subtitle("Available as the “Virtual Stream Mix” microphone in OBS, Discord, etc.")
            .build();
        stream_row.add_prefix(&gtk::Image::from_icon_name("audio-input-microphone-symbolic"));
        outputs_group.add(&stream_row);

        let channels_group = adw::PreferencesGroup::builder()
            .title("Channel Assignments")
            .build();
        let channel_rows: Vec<adw::ActionRow> = (0..CHANNEL_COUNT)
            .map(|_| {
                let row = adw::ActionRow::builder().subtitle("No input").build();
                channels_group.add(&row);
                row
            })
            .collect();

        let content = gtk::Box::builder()
            .orientation(gtk::Orientation::Vertical)
            .spacing(24)
            .margin_top(12)
            .margin_bottom(24)
            .margin_start(12)
            .margin_end(12)
            .build();
        content.append(&outputs_group);
        content.append(&channels_group);

        let root = gtk::ScrolledWindow::builder()
            .hscrollbar_policy(gtk::PolicyType::Never)
            .child(&content)
            .build();

        Self {
            root,
            monitor_row,
            stream_row,
            channel_rows,
        }
    }

    pub fn update(
        &self,
        config: &Config,
        monitor_device_label: &str,
        describe: &dyn Fn(&Assignment) -> String,
    ) {
        self.monitor_row
            .set_subtitle(&format!("Routed to {monitor_device_label}"));
        let _ = &self.stream_row;
        for (i, row) in self.channel_rows.iter().enumerate() {
            let ch = &config.channels[i];
            row.set_title(&glib::markup_escape_text(&ch.name));
            let subtitle = ch
                .assignment
                .as_ref()
                .map(describe)
                .unwrap_or_else(|| "No input".to_string());
            row.set_subtitle(&glib::markup_escape_text(&subtitle));
        }
    }
}

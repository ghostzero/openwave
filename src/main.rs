mod audio;
mod config;
mod ui;

use adw::prelude::*;
use gtk::glib;

pub const APP_ID: &str = "de.ghostzero.OpenWave";

fn main() -> glib::ExitCode {
    let app = adw::Application::builder().application_id(APP_ID).build();
    app.connect_startup(|_| ui::load_css());
    app.connect_activate(|app| {
        if let Some(window) = app.active_window() {
            window.present();
            return;
        }
        ui::window::build(app).present();
    });
    app.set_accels_for_action("window.close", &["<primary>q"]);
    app.run()
}

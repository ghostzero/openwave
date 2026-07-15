mod audio;
mod config;
mod ui;

use std::cell::Cell;
use std::rc::Rc;

use adw::prelude::*;
use gtk::glib;

pub const APP_ID: &str = "de.ghostzero.OpenWave";

fn main() -> glib::ExitCode {
    let app = adw::Application::builder().application_id(APP_ID).build();
    app.add_main_option(
        "hidden",
        glib::Char::from(0u8),
        glib::OptionFlags::NONE,
        glib::OptionArg::None,
        "Start with the window hidden (used by autostart)",
        None,
    );
    let start_hidden = Rc::new(Cell::new(false));
    {
        let start_hidden = start_hidden.clone();
        app.connect_handle_local_options(move |_, options| {
            if options.contains("hidden") {
                start_hidden.set(true);
            }
            std::ops::ControlFlow::Continue(()) // continue normal processing
        });
    }
    app.connect_startup(|_| ui::load_css());
    app.connect_activate(move |app| {
        // Closing only hides the window; re-activation brings it back.
        if let Some(window) = app
            .active_window()
            .or_else(|| app.windows().into_iter().next())
        {
            window.present();
            return;
        }
        let window = ui::window::build(app);
        // With --hidden the window is created but not shown: the virtual
        // devices come up in the background, the window appears on the next
        // activation.
        if !start_hidden.replace(false) {
            window.present();
        }
    });
    app.set_accels_for_action("app.quit", &["<primary>q"]);
    app.run()
}

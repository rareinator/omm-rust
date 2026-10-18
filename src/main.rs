mod hidpp;
mod onboard;
mod profile;
mod ui;

use gtk::glib;
use gtk::prelude::*;

const APP_ID: &str = "dev.omm.OnboardMemoryManager";

fn main() -> glib::ExitCode {
    let app = adw::Application::builder().application_id(APP_ID).build();
    app.connect_activate(ui::build);
    app.run()
}

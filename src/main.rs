mod hidpp;
mod onboard;
mod profile;
mod ui;

use gtk::glib;
use gtk::prelude::*;

const APP_ID: &str = "dev.omm.OnboardMemoryManager";

fn main() -> glib::ExitCode {
    // Our own flags are handled here; GApplication would reject unknown options.
    let (flags, gtk_args): (Vec<String>, Vec<String>) =
        std::env::args().partition(|a| a == "--dry-run");
    let dry_run = !flags.is_empty();

    let app = adw::Application::builder().application_id(APP_ID).build();
    app.connect_activate(move |app| ui::build(app, dry_run));
    app.run_with_args(&gtk_args)
}

use bnksound::app;
use gtk4 as gtk;
use gtk4::glib;
use gtk4::prelude::*;

const APP_ID: &str = "io.github.borgenk.BnkSound";

fn main() -> glib::ExitCode {
    gtk4::gdk::set_allowed_backends("wayland");

    let app = gtk::Application::builder().application_id(APP_ID).build();
    app.connect_activate(app::activate);
    app.run()
}

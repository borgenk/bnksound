use bnksound::APP_ID;
use bnksound::gtk_shell::app;
use gtk4 as gtk;
use gtk4::glib;
use gtk4::prelude::*;

fn main() -> glib::ExitCode {
    gtk4::gdk::set_allowed_backends("wayland");

    let app = gtk::Application::builder().application_id(APP_ID).build();
    app.connect_activate(app::activate);
    app.run()
}

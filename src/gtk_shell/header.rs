//! The window's header bar.
//!
//! GTK paints it and the desktop's theme styles it, so nothing here sets a
//! colour, a size, or a font. Setting a titlebar at all is what earns that:
//! a window without one gets the bar GTK falls back to, which themes mark
//! .default-decoration and style down as chrome for an app that never asked.
//!
//! The profile selector takes the title's place, which is what keeps the
//! window down to a single bar.

use gtk4 as gtk;
use gtk4::prelude::*;

/// Build the header around a leading widget and hand it to the window.
pub fn install(window: &gtk::ApplicationWindow, leading: &impl IsA<gtk::Widget>) {
    let header = gtk::HeaderBar::new();
    // Packed at the start, where the native shell paints its chip, and after
    // whatever window buttons the layout puts on that side. The empty title
    // widget is what keeps GTK from filling the middle with the window name.
    header.pack_start(leading);
    header.set_title_widget(Some(&gtk::Label::new(None)));
    window.set_titlebar(Some(&header));
    follow_decoration_layout(&header);
}

/// Follow the desktop's button layout, minus the window icon.
///
/// Leaving the property unset would honour the layout whole, icon included.
/// Pinning one would override the user's button set and which side it sits on,
/// which is the theme's call and not ours. Rewriting only the icon out of
/// whatever the setting says keeps both.
fn follow_decoration_layout(header: &gtk::HeaderBar) {
    let Some(settings) = gtk::Settings::default() else {
        return;
    };
    apply(&settings, header);

    let header = header.clone();
    settings.connect_gtk_decoration_layout_notify(move |settings| apply(settings, &header));
}

fn apply(settings: &gtk::Settings, header: &gtk::HeaderBar) {
    let layout = settings.gtk_decoration_layout();
    header.set_decoration_layout(layout.as_deref().map(without_icon).as_deref());
}

/// Drop the icon element from a decoration layout.
///
/// The string is sides split by a colon, each a comma-separated list of
/// elements, and GTK splits at the first colon only. Filtering elements in
/// place leaves the colons where they were, so a layout naming no side at all
/// still names none afterwards.
fn without_icon(layout: &str) -> String {
    layout
        .split(':')
        .map(|side| {
            side.split(',')
                .filter(|element| *element != "icon")
                .collect::<Vec<_>>()
                .join(",")
        })
        .collect::<Vec<_>>()
        .join(":")
}

#[cfg(test)]
mod tests {
    use super::without_icon;

    #[test]
    fn drops_the_icon_and_leaves_the_split_alone() {
        assert_eq!(
            without_icon("icon:minimize,maximize,close"),
            ":minimize,maximize,close"
        );
    }

    #[test]
    fn keeps_a_layout_that_never_named_an_icon() {
        assert_eq!(without_icon("appmenu:close"), "appmenu:close");
        assert_eq!(without_icon(":minimize,close"), ":minimize,close");
    }

    #[test]
    fn drops_the_icon_from_whichever_side_carries_it() {
        assert_eq!(without_icon("close:icon"), "close:");
        assert_eq!(without_icon("minimize,icon:close"), "minimize:close");
    }

    /// A layout with no colon puts the same buttons on both sides, so the
    /// rewrite must not introduce one.
    #[test]
    fn a_sideless_layout_stays_sideless() {
        assert_eq!(without_icon("icon,close"), "close");
    }

    /// GTK reads an empty layout as "no controls at all", which is the honest
    /// answer when the icon was the only thing named.
    #[test]
    fn an_icon_only_layout_empties_out() {
        assert_eq!(without_icon("icon"), "");
    }
}

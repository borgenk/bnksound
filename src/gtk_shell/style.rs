//! The GTK build's own CSS, all of it.
//!
//! Two sheets go in together. The base one is always on and carries the little
//! that no widget property can express. The chrome one repaints GTK's header,
//! window, and menus in the mixer's colours, so the GTK build reads like the
//! native shell rather than like the desktop's other apps. It is on unless
//! `gtk_chrome theme` hands the chrome back to the desktop.
//!
//! Every colour is written out of [`Palette`], so the CSS and the painted body
//! cannot drift apart. Nothing here is typed as a hex literal.
//!
//! Both go in above the theme, because a rule the theme can silently drop is
//! not worth shipping, and below a user's own gtk.css, which still wins.

use gtk4 as gtk;
use gtk4::gdk;

use crate::render::buffer::Color;
use crate::settings::GtkChrome;
use crate::ui::theme::Palette;

/// Scopes the menu rules to the profile selector's list, so they reach nothing
/// else here and nothing in any other app. Prefixed because a bare name could
/// collide with a class some theme already uses.
pub const MENU_CLASS: &str = "bnksound-profile-menu";

/// Marks the row of the active profile. Prefixed for the same reason: themes
/// have their own opinions about a bare `.active`.
pub const ACTIVE_CLASS: &str = "bnksound-active";

/// Scopes the header button's rules to the profile selector.
pub const BUTTON_CLASS: &str = "bnksound-profile-button";

/// Corner radius on a menu row's hover, in logical pixels.
const MENU_ROW_RADIUS: i32 = 6;

/// The window's drop shadow, focused and not. Deliberately not palette colours:
/// a shadow is the absence of light under a floating window, not part of the
/// mixer's own scheme, and it has to read against whatever is behind the window
/// rather than against anything the app paints.
const SHADOW: &str = "rgba(0,0,0,0.5)";
const SHADOW_BACKDROP: &str = "rgba(0,0,0,0.2)";

/// Register the app's stylesheet with the display.
///
/// Re-registering the same provider replaces it rather than stacking, so this
/// is safe to call more than once.
pub fn install(palette: &Palette, chrome: GtkChrome) {
    let Some(display) = gdk::Display::default() else {
        return;
    };

    let mut css = base_css();
    if chrome == GtkChrome::Palette {
        css.push_str(&chrome_css(palette));
    }

    let provider = gtk::CssProvider::new();
    provider.load_from_data(&css);
    gtk::style_context_add_provider_for_display(
        &display,
        &provider,
        gtk::STYLE_PROVIDER_PRIORITY_APPLICATION,
    );
}

/// What holds whatever the theme is: the rounded hover on menu rows, which has
/// no widget property behind it. One property only, so a row's colours, hover
/// shade, and focus ring all stay the theme's.
fn base_css() -> String {
    format!(
        ".{MENU_CLASS} > row {{
  border-radius: {MENU_ROW_RADIUS}px;
}}

/* Pressing the selector, and holding it open, both sit a solid block behind
   the label in most themes. The menu dropping into view says the click landed,
   so the block only reads as the button denting inward. Hover is left alone:
   that is the part that says the button is a button. */
.{BUTTON_CLASS} > button:active,
.{BUTTON_CLASS} > button:checked {{
  background: none;
  box-shadow: none;
}}

/* The ring a click leaves behind. GTK hands focus back to the button when the
   popover closes, so focus-on-click being off does not prevent it, and themes
   draw it as some mix of outline, border, and shadow depending on state. All
   three go, in every state, because the label and the menu are the feedback
   this control needs. Border colour rather than width, so the button does not
   change size as it loses it. */
.{BUTTON_CLASS} > button {{
  outline: none;
  box-shadow: none;
  border-color: transparent;
}}
"
    )
}

/// The mixer's palette over GTK's chrome: the header's colour, borderless
/// edges, and a menu that matches the body underneath it.
fn chrome_css(p: &Palette) -> String {
    let titlebar = rgba(p.titlebar);
    let bg = rgba(p.bg);
    let surface = rgba(p.surface);
    let border = rgba(p.border);
    let text = rgba(p.text);
    let subtle = rgba(p.text_subtle);
    let hover = rgba(p.wash_8);
    let accent = rgba(p.accent);

    format!(
        "headerbar.titlebar {{
  background: {titlebar};
  color: {text};
  border: none;
  box-shadow: none;
  min-height: 0;
}}

/* An unfocused window dims its title in most themes. The native shell draws
   the same strip either way, so only the text follows focus here. */
headerbar.titlebar:backdrop {{
  background: {titlebar};
  color: {subtle};
}}

window.background {{
  background: {bg};
  color: {text};
}}

/* The window's own edge. Adwaita draws it as a 1px spread ring in a second
   box-shadow layer, not as a border, so there is no border property to unset:
   respecifying the shadow without that layer is what removes it. The blur
   stays, since that is what separates a floating window from what is behind
   it. `border: none` is here for themes that do use a border. */
window.csd {{
  border: none;
  box-shadow: 0 3px 9px 1px {SHADOW};
}}

window.csd:backdrop {{
  box-shadow: 0 2px 6px 2px {SHADOW_BACKDROP};
}}

/* The same panel the native menu paints: surface fill, a hairline border, and
   the same corner. Its border stays where the window's went, because here it
   is what separates the menu from the mixer behind it. */
popover > contents {{
  background: {surface};
  color: {text};
  border: 1px solid {border};
  border-radius: {MENU_ROW_RADIUS}px;
  box-shadow: none;
  padding: 0;
}}

.{MENU_CLASS} {{
  background: transparent;
}}

.{MENU_CLASS} > row:hover {{
  background: {hover};
}}

/* The native menu names the active profile in the accent rather than sitting
   it on a highlight, which leaves the row under the pointer the only lit one. */
.{MENU_CLASS} > row.{ACTIVE_CLASS} label {{
  color: {accent};
}}

.{MENU_CLASS} separator {{
  background: {border};
}}
"
    )
}

/// A palette colour as a CSS rgba() literal.
fn rgba(c: Color) -> String {
    let a = f32::from(c.a) / 255.0;
    format!("rgba({},{},{},{a:.3})", c.r, c.g, c.b)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn opaque_colour_becomes_a_full_alpha_literal() {
        assert_eq!(rgba(Color::rgb(0x29, 0x2b, 0x30)), "rgba(41,43,48,1.000)");
    }

    #[test]
    fn a_wash_keeps_its_alpha() {
        assert_eq!(
            rgba(Color::rgba(255, 255, 255, 20)),
            "rgba(255,255,255,0.078)"
        );
    }

    /// The base sheet is what every run gets, so it must not name a colour: a
    /// theme's own colours have to keep coming through. Removing a background
    /// is fine; picking one is not.
    #[test]
    fn the_base_sheet_names_no_colours() {
        let css = base_css();
        assert!(css.contains("border-radius"));
        assert!(!css.contains("rgba("), "base sheet: {css}");
        assert!(!css.contains('#'), "base sheet: {css}");
    }

    /// Both sheets reach only this app's own widgets, so a bare `button` or
    /// `row` selector would be a bug: it would restyle every one in the process.
    #[test]
    fn every_rule_is_scoped_to_one_of_our_classes() {
        let css = format!("{}{}", base_css(), chrome_css(&Palette::dark()));
        let ours = [MENU_CLASS, ACTIVE_CLASS, BUTTON_CLASS];
        for selector in css
            .lines()
            .filter(|l| l.contains('{') || l.trim_end().ends_with(','))
            .filter(|l| l.contains("button") || l.contains("row"))
        {
            assert!(
                ours.iter().any(|c| selector.contains(c)),
                "unscoped selector: {selector}"
            );
        }
    }

    /// Every colour in the chrome sheet comes from the palette, so a value
    /// typed by hand would show up as a literal that is not an rgba() call.
    #[test]
    fn the_chrome_sheet_names_palette_colours_only() {
        let css = chrome_css(&Palette::dark());
        assert!(!css.contains('#'), "hex literal in chrome sheet: {css}");
        assert!(css.contains(&rgba(Palette::dark().titlebar)));
        assert!(css.contains(&rgba(Palette::dark().surface)));
    }

    #[test]
    fn the_chrome_sheet_scopes_its_menu_rules() {
        let css = chrome_css(&Palette::dark());
        for line in css.lines().filter(|l| l.contains("row:hover")) {
            assert!(line.contains(MENU_CLASS), "unscoped row rule: {line}");
        }
    }
}

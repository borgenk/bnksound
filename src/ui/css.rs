use gtk4 as gtk;

/// The static stylesheet, every colour written as an `@bnk_*` reference.
/// [`install_css`] prepends the matching `@define-color` header from a
/// [`super::theme::Palette`]; retint via the palette, not this template.
const CSS_TEMPLATE: &str = r#"
/* Chrome surfaces (window, titlebar, action bar, popovers, modals) are
   deliberately locked to a fixed dark palette, giving up GTK theme-tracking
   so the app always reads as bnklab-native. Don't "fix" this to track
   @theme_* colours; retint via theme::Palette instead. */
window {
    background-color: @bnk_bg;
    color: @bnk_text;
}
/* Borderless mode (class `bnk-no-window-border`): drop every chrome layer
   themes use for a window outline. Both `border` and `box-shadow` (themes
   draw the outline as a shadow) plus the `decoration` drop shadow, all in
   their `:backdrop` variants too so neither focus state shows it. */
window.bnk-no-window-border,
window.bnk-no-window-border:backdrop {
    border: none;
    box-shadow: none;
}
window.bnk-no-window-border decoration,
window.bnk-no-window-border:backdrop decoration {
    border: none;
    box-shadow: none;
    border-radius: 0;
}
.bnk-subtle { font-size: 12px; }
.bnk-active { color: @bnk_accent; }
.bnk-inactive { color: @bnk_text_muted; }
.bnk-tombstoned { color: @bnk_text_muted; }
/* Sink column heading: all-caps category label. Inherits color so
   `.bnk-active`/`.bnk-inactive` can paint it pink/gray for the default sink. */
.bnk-device-type {
    font-size: 12px;
    font-weight: bold;
    letter-spacing: 1px;
}
/* Device name under the type label, subdued so the heading dominates. */
.bnk-device-name {
    font-size: 10px;
    color: @bnk_text_subtle;
}
/* The `%` symbol and volume number track the slider fill: green normally,
   amber past unity gain. The symbol's smaller font is set in row.rs. */
.bnk-percent-sym,
.bnk-percent-num {
    color: @bnk_volume_ok;
}
.bnk-percent-sym.bnk-percent-warning,
.bnk-percent-num.bnk-percent-warning {
    color: @bnk_warning;
}
/* Volume percentage drawn on the slider thumb: small, bold, muted gray so
   it reads as quiet detail on the dark knob (amber past unity). Zeroed
   padding/min-size so the label hugs the digits and centers on the knob. */
.bnk-knob-value {
    font-size: 9px;
    font-weight: bold;
    color: @bnk_text_muted;
    padding: 0;
    margin: 0;
    min-width: 0;
    min-height: 0;
}
.bnk-knob-value.bnk-knob-value-warning {
    color: @bnk_warning;
}
/* Minimal pin-picker buttons: faded border, small label, no button chrome.
   The active selection gets the pink accent border + matching text. */
.bnk-pick {
    border: 1px solid @bnk_wash_18;
    background: transparent;
    background-image: none;
    box-shadow: none;
    padding: 1px 2px;
    font-size: 12px;
    border-radius: 4px;
    color: @bnk_text_subtle;
    min-height: 0;
    /* Fade the lit/unlit and hover transitions rather than snap. */
    transition: background 120ms ease, border-color 120ms ease, color 120ms ease;
    /* Floor for single-letter labels so they read as small squares;
       multi-character labels grow past it from their content width. */
    min-width: 16px;
}
.bnk-pick:hover {
    background: @bnk_wash_5;
    background-image: none;
}
.bnk-pick:active {
    background: @bnk_wash_8;
    background-image: none;
}
/* Strip the theme's accent-blue focus ring on the action-bar picks. */
.bnk-pick:focus,
.bnk-pick:focus-visible,
.bnk-pick:focus-within {
    outline: none;
    outline-width: 0;
    outline-color: transparent;
    box-shadow: none;
}
.bnk-pick.bnk-pick-active {
    border-color: @bnk_accent;
    color: @bnk_accent;
}
/* Section-filter toggles (IN/OUT/APP) reuse `.bnk-pick` but flip
   `.bnk-active`/`.bnk-inactive` in `refresh_lists`. The two-class
   `.bnk-pick.bnk-active` (0,2,0) is needed so the lit state outranks
   `.bnk-pick`. Lit = solid yellow fill + opaque white label (translucent
   text washed out against the fill); yellow (not brand pink) keeps the
   filters reading as a distinct control. */
.bnk-pick.bnk-active {
    border-color: @bnk_filter;
    background: @bnk_filter;
    color: @bnk_on_filled;
}
.bnk-pick.bnk-active:hover {
    background: @bnk_filter_hover;
    border-color: @bnk_filter_hover;
}
/* Subtle low-contrast wash on expanded sub-row columns so a parent + its
   children read as one app. A quiet grouping cue, not a highlight. */
.bnk-app-member {
    background-color: @bnk_wash_4;
}
/* Vertical column dividers, dropped to a barely-there hairline (GTK's
   default ~15% white separator reads too strong in this dense layout). */
separator.bnk-col-sep {
    background-color: @bnk_wash_6;
}
.bnk-palette-backdrop {
    background-color: @bnk_backdrop;
}
/* Command palette overlay (chrome palette, per the decision above). The
   panel sits on the form-field shade so the search field and command list
   read as one uninterrupted surface. */
.bnk-palette-panel {
    background-color: @bnk_field_bg;
    border-radius: 8px;
}
/* Search field: same shade as the panel, borderless, no accent-blue
   focus ring (themes draw it via box-shadow and/or outline). */
.bnk-palette-panel entry {
    background-color: @bnk_field_bg;
    background-image: none;
    border: none;
    box-shadow: none;
    border-radius: 6px;
    padding: 6px 10px;
    min-height: 0;
    color: @bnk_text;
}
.bnk-palette-panel entry:focus,
.bnk-palette-panel entry:focus-within,
.bnk-palette-panel entry:focus-visible {
    border: none;
    box-shadow: none;
    outline: none;
    outline-width: 0;
    outline-color: transparent;
}
/* Transparent list/scroll surfaces so the panel shade carries the whole
   overlay (some themes otherwise paint `list` their view bg). */
.bnk-palette-panel scrolledwindow,
.bnk-palette-panel list {
    background-color: transparent;
}
.bnk-palette-empty {
    color: @bnk_text_muted;
    padding: 12px 14px;
    font-size: 12px;
}
.bnk-palette-row {
    padding: 8px 14px;
}
/* Suppress the theme's hover background on activatable rows; only the
   keyboard/click selection should highlight. */
.bnk-palette-row:hover {
    background-color: transparent;
    background-image: none;
}
.bnk-palette-row:selected,
.bnk-palette-row:selected:hover {
    background-color: @bnk_wash_10;
    color: inherit;
}
/* Strip the theme's accent-blue focus ring on rows. */
.bnk-palette-row:focus,
.bnk-palette-row:focus-visible,
.bnk-palette-row:focus-within {
    outline: none;
    outline-width: 0;
    outline-color: transparent;
    box-shadow: none;
}
/* Global scrollbars: pin the slider to a constant slim pill in every state
   (no widen/recolour on hover/drag), with only a faint brighten on hover.
   Scoped to `scrollbar` so the volume `.bnk-scale` is untouched. */
scrollbar,
scrollbar trough {
    background: transparent;
    border: none;
    margin: 0;
}
scrollbar slider {
    min-width: 4px;
    min-height: 4px;
    border: none;
    border-radius: 4px;
    margin: 2px;
    background-color: @bnk_wash_20;
    transition: background-color 120ms ease;
}
scrollbar slider:hover,
scrollbar.hovering slider {
    background-color: @bnk_wash_30;
}
scrollbar slider:active,
scrollbar.dragging slider {
    background-color: @bnk_wash_30;
    border: none;
}
/* No edge overshoot glow or undershoot fade, so the headerbar/body seam
   stays static when a scroll bottoms out. */
overshoot,
undershoot {
    background: none;
    box-shadow: none;
}
/* Left action bar: narrow vertical strip of shortcut buttons, on the
   secondary surface per the chrome-palette decision above. */
.bnk-action-bar {
    background-color: @bnk_surface;
    padding: 12px 6px;
}
/* Titlebar: on the titlebar surface per the chrome-palette decision above.
   border-bottom / box-shadow removal flattens the seam against the body. */
headerbar {
    background-color: @bnk_titlebar;
    /* Clear the theme's focused-headerbar gradient so it doesn't read as a
       muddy highlight band over our flat color. */
    background-image: none;
    color: @bnk_text;
    border-bottom: none;
    box-shadow: none;
    padding: 0 6px;
    /* No `min-height`: the WindowControls buttons set the natural floor and
       shrinking past it just clips icons. */
}
/* In-window titlebar strip: shown instead of the HeaderBar when `titlebar
   strip` is set (or GTK_CSD=0). Same titlebar surface as `headerbar`;
   `min-height` sets the height since there are no titlebutton glyphs. */
.bnk-titlebar-strip {
    background-color: @bnk_titlebar;
    background-image: none;
    /* No vertical padding: `min-height` alone pins the ~34 px height; the
       profile button centers in the slack (left edge trimmed tighter). */
    padding: 0 6px 0 4px;
    min-height: 34px;
}
/* Slim the profile button inside the strip so it fits the 34 px bar. */
.bnk-titlebar-strip menubutton.bnk-profile-menu-btn > button {
    padding: 4px 12px;
}
/* Profile menu button: a "pill" in the titlebar. Subtle wash always
   visible, stronger on hover / when the popover is open. */
menubutton.bnk-profile-menu-btn > button {
    background-color: @bnk_wash_4;
    background-image: none;
    border: none;
    box-shadow: none;
    padding: 8px 12px;
    border-radius: 4px;
    color: @bnk_text;
    min-height: 0;
    font-size: 13px;
}
menubutton.bnk-profile-menu-btn > button:hover,
menubutton.bnk-profile-menu-btn > button:active,
menubutton.bnk-profile-menu-btn:checked > button {
    background-color: @bnk_wash_8;
    color: @bnk_text;
}
/* Strip the theme's accent-blue focus ring (outline and/or box-shadow). */
menubutton.bnk-profile-menu-btn > button:focus,
menubutton.bnk-profile-menu-btn > button:focus-visible,
menubutton.bnk-profile-menu-btn > button:focus-within {
    outline: none;
    outline-width: 0;
    outline-color: transparent;
    box-shadow: none;
}
/* Profile popover panel. `> contents` is the GTK4 inner node that draws
   the panel background. */
.bnk-profile-popover > contents {
    background-color: @bnk_surface;
    border: 1px solid @bnk_border;
    border-radius: 6px;
    padding: 1px;
    box-shadow: none;
}
/* `.bnk-profile-popover .bnk-profile-row` (0,2,0) beats Adwaita's
   `popover button`. Resets inner label margin/padding too, else Adwaita's
   chrome leaves a tiny hover pill inside a taller perceived row. */
.bnk-profile-popover .bnk-profile-row {
    background: transparent;
    background-image: none;
    border: none;
    box-shadow: none;
    border-radius: 3px;
    padding: 4px 10px;
    margin: 0;
    color: @bnk_text;
    min-height: 0;
    font-size: 12px;
}
.bnk-profile-popover .bnk-profile-row > * {
    margin: 0;
    padding: 0;
    min-height: 0;
}
.bnk-profile-popover .bnk-profile-row:hover {
    background-color: @bnk_wash_8;
}
.bnk-profile-row.bnk-profile-row-active {
    color: @bnk_accent;
}
.bnk-profile-row.bnk-profile-row-new {
    color: @bnk_text_idle;
}
/* Strip the theme's accent-blue focus ring on popover rows. */
.bnk-profile-row:focus,
.bnk-profile-row:focus-visible,
.bnk-profile-row:focus-within {
    outline: none;
    outline-width: 0;
    outline-color: transparent;
    box-shadow: none;
}
.bnk-profile-popover-sep {
    background-color: @bnk_border;
    min-height: 1px;
    margin: 0;
}
/* Profile-management modal (chrome palette, per the decision above): panel
   on the window background, name entry on the form-field shade, primary
   button on the CTA shade. */
.bnk-modal-panel {
    background-color: @bnk_bg;
    border-radius: 8px;
    padding: 16px;
}
.bnk-modal-title {
    font-weight: bold;
    font-size: 14px;
    color: @bnk_text;
}
.bnk-modal-body {
    color: @bnk_text_modal_body;
    font-size: 12px;
}
.bnk-modal-error {
    color: @bnk_warning;
    font-size: 12px;
}
/* Name entry: flat form field, no frame, no accent-blue focus ring. */
.bnk-modal-panel entry {
    background-color: @bnk_field_bg;
    background-image: none;
    border: none;
    box-shadow: none;
    border-radius: 6px;
    padding: 6px 10px;
    min-height: 0;
    color: @bnk_text;
}
.bnk-modal-panel entry:focus,
.bnk-modal-panel entry:focus-within,
.bnk-modal-panel entry:focus-visible {
    border: none;
    box-shadow: none;
    outline: none;
    outline-width: 0;
    outline-color: transparent;
}
/* Action buttons: flat secondary (Cancel), .suggested-action primary on the
   CTA shade, .destructive-action danger on a muted dark red. */
.bnk-modal-panel button {
    background-color: @bnk_wash_4;
    background-image: none;
    border: none;
    box-shadow: none;
    border-radius: 6px;
    padding: 6px 14px;
    min-height: 0;
    font-size: 13px;
    color: @bnk_text;
}
.bnk-modal-panel button:hover,
.bnk-modal-panel button:active {
    background-color: @bnk_wash_8;
    color: @bnk_text;
}
.bnk-modal-panel button:focus,
.bnk-modal-panel button:focus-within,
.bnk-modal-panel button:focus-visible {
    outline: none;
    outline-width: 0;
    outline-color: transparent;
    box-shadow: none;
}
.bnk-modal-panel button.suggested-action {
    background-color: @bnk_cta_bg;
    color: @bnk_on_filled;
}
.bnk-modal-panel button.suggested-action:hover,
.bnk-modal-panel button.suggested-action:active {
    background-color: @bnk_cta_bg_hover;
    color: @bnk_on_filled;
}
.bnk-modal-panel button.destructive-action {
    background-color: @bnk_danger_bg;
    color: @bnk_text;
}
.bnk-modal-panel button.destructive-action:hover,
.bnk-modal-panel button.destructive-action:active {
    background-color: @bnk_danger_bg_hover;
    color: @bnk_text;
}
.bnk-scale trough {
    min-width: 16px;
    /* Just shy of square, matching the peak meter's segments. */
    border-radius: 3px;
    /* @bnk_dim_grid matches the meter's unlit grid so the empty trough
       reads as the same "off" surface as the meter background. */
    background-color: @bnk_dim_grid;
    background-image: none;
}
.bnk-scale highlight {
    min-width: 16px;
    border-radius: 3px;
    background-color: @bnk_scale_fill;
    background-image: none;
}
.bnk-scale slider {
    /* 28 px knob with `margin: -6px` keeps the net allocation at 16 px
       (the trough's min-width), so the value-bearing range is unchanged. */
    min-width: 28px;
    min-height: 28px;
    border-radius: 9999px;
    background-color: @bnk_surface;
    background-image: none;
    border: none;
    box-shadow: 0 1px 2px @bnk_shadow_strong, 0 2px 4px @bnk_shadow_soft;
    margin: -6px;
    padding: 0;
}
.bnk-scale.bnk-scale-warning slider,
.bnk-scale.bnk-scale-warning slider:hover,
.bnk-scale.bnk-scale-warning slider:focus,
.bnk-scale.bnk-scale-warning slider:focus-visible,
.bnk-scale.bnk-scale-warning slider:active {
    /* Warning only re-tints the highlight fill; the knob keeps its resting
       color and shadow across every pseudo-state, so themes can't sneak in
       a focus accent. */
    background-color: @bnk_surface;
    background-image: none;
    border: none;
    outline: none;
    box-shadow: 0 1px 2px @bnk_shadow_strong, 0 2px 4px @bnk_shadow_soft;
}
.bnk-scale.bnk-scale-warning highlight {
    background-color: @bnk_scale_fill_warning;
    background-image: none;
}
/* Nuke every focus indicator (box-shadow or outline) the theme might
   apply to the scale or its parts, on every relevant element/state. */
.bnk-scale,
.bnk-scale *,
.bnk-scale:focus,
.bnk-scale:focus-visible,
.bnk-scale:focus-within,
.bnk-scale *:focus,
.bnk-scale *:focus-visible,
.bnk-scale *:focus-within {
    outline: none;
    outline-width: 0;
    outline-color: transparent;
}
.bnk-scale trough,
.bnk-scale trough:focus,
.bnk-scale trough:focus-visible,
.bnk-scale trough:hover,
.bnk-scale highlight,
.bnk-scale highlight:focus,
.bnk-scale highlight:focus-visible {
    box-shadow: none;
    /* Kill the theme's accent-color border around the highlight (a blue
       rim around our fill). */
    border: none;
    /* Force margin to 0: a theme negative-margin on highlight doubles in
       the measure pass into a "reported min height -2" GTK warning. */
    margin: 0;
    padding: 0;
    min-height: 0;
}
.bnk-scale slider:focus,
.bnk-scale slider:focus-visible {
    /* Re-assert the resting drop shadow so focus doesn't strip it. */
    box-shadow: 0 1px 2px @bnk_shadow_strong, 0 2px 4px @bnk_shadow_soft;
}
"#;

pub(super) fn install_css(palette: &crate::ui::theme::Palette) {
    // Prepend the palette's `@define-color` block so every `@bnk_*`
    // reference resolves (GTK requires colours be defined before use).
    let css = format!("{}\n{CSS_TEMPLATE}", palette.define_colors());
    let provider = gtk::CssProvider::new();
    provider.load_from_data(&css);
    if let Some(display) = gtk::gdk::Display::default() {
        gtk::style_context_add_provider_for_display(
            &display,
            &provider,
            gtk::STYLE_PROVIDER_PRIORITY_APPLICATION,
        );
    }
}

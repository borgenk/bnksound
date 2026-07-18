//! The shared mixer palette, expressed in renderer colors.
//!
//! These are the same values the GTK reference theme carries, transcribed into
//! render::Color (8-bit straight alpha) for the software renderer. The float
//! alphas and cairo tints round to 8 bits here; the difference is below one
//! step per channel and not visible. The GTK theme keeps its own f32 copy for
//! CSS and cairo until the GTK body moves onto this renderer, at which point the
//! two collapse into this one.

use crate::render::buffer::Color;

/// Opaque color from 8-bit channels.
const fn rgb(r: u8, g: u8, b: u8) -> Color {
    Color::rgb(r, g, b)
}

/// 8-bit channels with a fractional alpha, rounded to 8 bits.
fn rgba(r: u8, g: u8, b: u8, a: f32) -> Color {
    Color::rgba(r, g, b, (a * 255.0).round() as u8)
}

/// Fractional channels (a cairo-native tint), rounded to 8 bits.
fn tint(r: f32, g: f32, b: f32, a: f32) -> Color {
    Color::rgba(
        (r * 255.0).round() as u8,
        (g * 255.0).round() as u8,
        (b * 255.0).round() as u8,
        (a * 255.0).round() as u8,
    )
}

/// Every color the mixer body paints, named by role. Copy, so it threads
/// through the render chain by value.
#[derive(Clone, Copy)]
pub struct Palette {
    // Chrome surfaces.
    pub bg: Color,
    pub surface: Color,
    pub titlebar: Color,
    pub field_bg: Color,
    // Text hierarchy.
    pub text: Color,
    pub text_subtle: Color,
    pub text_muted: Color,
    pub text_modal_body: Color,
    pub text_idle: Color,
    pub on_filled: Color,
    // Accent and status. The brand pink (accent) is only ever the
    // active/selected state, never structural chrome.
    pub accent: Color,
    pub warning: Color,
    pub volume_ok: Color,
    pub filter: Color,
    pub filter_hover: Color,
    // Borders.
    pub border: Color,
    // Modal action buttons.
    pub cta_bg: Color,
    pub cta_bg_hover: Color,
    pub danger_bg: Color,
    pub danger_bg_hover: Color,
    // Volume slider fill.
    pub scale_fill: Color,
    pub scale_fill_warning: Color,
    // Ring drawn around the knob while hovered or dragged.
    pub knob_halo: Color,
    // Contrast washes, named by alpha percentage. dim_grid is the faintest,
    // shared by the slider trough and the meter's unlit grid.
    pub dim_grid: Color,
    pub wash_4: Color,
    pub wash_5: Color,
    pub wash_6: Color,
    pub wash_8: Color,
    pub wash_10: Color,
    pub wash_18: Color,
    pub wash_20: Color,
    pub wash_30: Color,
    // Overlays.
    pub backdrop: Color,
    pub shadow_strong: Color,
    pub shadow_soft: Color,
    // Level meter tints. Deliberately distinct from the slider fill: these read
    // "how loud", not "what gain".
    pub meter_red: Color,
    pub meter_amber: Color,
    pub meter_green: Color,
    pub meter_neutral: Color,
    // Unity-gain reference notch on the slider trough.
    pub unity_notch: Color,
}

impl Palette {
    /// The dark palette (the only theme today).
    pub fn dark() -> Self {
        Self {
            bg: rgb(0x1f, 0x21, 0x24),
            surface: rgb(0x26, 0x29, 0x2e),
            titlebar: rgb(0x29, 0x2b, 0x30),
            field_bg: rgb(0x2c, 0x2f, 0x34),
            text: rgb(0xec, 0xec, 0xec),
            text_subtle: rgb(0xaa, 0xaa, 0xaa),
            text_muted: rgb(0x88, 0x88, 0x88),
            text_modal_body: rgb(0xcc, 0xcc, 0xcc),
            text_idle: rgb(0x6b, 0x69, 0x69),
            on_filled: rgb(0xff, 0xff, 0xff),
            accent: rgb(0xff, 0x00, 0xaa),
            warning: rgb(0xff, 0x70, 0x43),
            volume_ok: rgb(0x66, 0xbb, 0x6a),
            filter: rgb(0xe5, 0xa9, 0x21),
            filter_hover: rgb(0xd6, 0x9e, 0x2e),
            border: rgb(0x33, 0x33, 0x33),
            cta_bg: rgb(0x31, 0x38, 0x44),
            cta_bg_hover: rgb(0x3d, 0x44, 0x50),
            danger_bg: rgb(0x5a, 0x2d, 0x2d),
            danger_bg_hover: rgb(0x6e, 0x36, 0x36),
            scale_fill: rgb(0x4c, 0xaf, 0x50),
            scale_fill_warning: rgb(0xf4, 0x51, 0x1e),
            knob_halo: rgba(0xff, 0xff, 0xff, 0.07),
            dim_grid: rgba(0xff, 0xff, 0xff, 0.03),
            wash_4: rgba(0xff, 0xff, 0xff, 0.04),
            wash_5: rgba(0xff, 0xff, 0xff, 0.05),
            wash_6: rgba(0xff, 0xff, 0xff, 0.06),
            wash_8: rgba(0xff, 0xff, 0xff, 0.08),
            wash_10: rgba(0xff, 0xff, 0xff, 0.10),
            wash_18: rgba(0xff, 0xff, 0xff, 0.18),
            wash_20: rgba(0xff, 0xff, 0xff, 0.20),
            wash_30: rgba(0xff, 0xff, 0xff, 0.30),
            backdrop: rgba(0x00, 0x00, 0x00, 0.5),
            shadow_strong: rgba(0x00, 0x00, 0x00, 0.25),
            shadow_soft: rgba(0x00, 0x00, 0x00, 0.18),
            meter_red: tint(1.0, 0.30, 0.30, 0.95),
            meter_amber: tint(1.0, 0.78, 0.30, 0.95),
            meter_green: tint(0.30, 0.80, 0.45, 0.95),
            meter_neutral: tint(0.55, 0.65, 0.78, 0.85),
            unity_notch: tint(0.6, 0.6, 0.6, 0.55),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn opaque_roles_are_fully_opaque() {
        let p = Palette::dark();
        assert_eq!(p.bg, Color::rgb(0x1f, 0x21, 0x24));
        assert_eq!(p.accent, Color::rgb(0xff, 0x00, 0xaa));
        assert_eq!(p.text.a, 255);
    }

    #[test]
    fn washes_round_alpha_to_eight_bits() {
        let p = Palette::dark();
        // 0.08 * 255 = 20.4 -> 20; 0.30 * 255 = 76.5 -> 77.
        assert_eq!(p.wash_8.a, 20);
        assert_eq!(p.wash_30.a, 77);
        assert_eq!(p.backdrop, Color::rgba(0, 0, 0, 128));
    }

    #[test]
    fn meter_tints_round_channels() {
        let p = Palette::dark();
        // 0.30 -> 77, 0.80 -> 204, 0.45 -> 115, 0.95 -> 242.
        assert_eq!(p.meter_green, Color::rgba(77, 204, 115, 242));
    }
}

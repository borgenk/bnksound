//! Central colour palette: the single source of truth for every colour
//! bnksound paints, through CSS or hand-drawn with cairo. CSS widgets read
//! a generated `@define-color` header ([`Palette::define_colors`]); the cairo
//! meter and slider notch read [`Color`] fields via [`Color::rgba_f64`].

use std::fmt::Write as _;

/// A single sRGB colour. Channels stored as `0.0..=1.0` floats so cairo
/// tints stay exact; CSS emission rounds back to 8-bit hex losslessly.
#[derive(Clone, Copy)]
pub(super) struct Color {
    r: f32,
    g: f32,
    b: f32,
    a: f32,
}

impl Color {
    /// Opaque colour from 8-bit channels (mirrors a CSS `#rrggbb`).
    fn hex(r: u8, g: u8, b: u8) -> Self {
        Self::hexa(r, g, b, 1.0)
    }

    /// 8-bit channels with explicit alpha (mirrors a CSS `rgba(...)`).
    fn hexa(r: u8, g: u8, b: u8, a: f32) -> Self {
        Self {
            r: r as f32 / 255.0,
            g: g as f32 / 255.0,
            b: b as f32 / 255.0,
            a,
        }
    }

    /// Cairo-native colour from `0.0..=1.0` channels (kept exact, no hex round-trip).
    fn rgbaf(r: f32, g: f32, b: f32, a: f32) -> Self {
        Self { r, g, b, a }
    }

    /// `(r, g, b, a)` in `0.0..=1.0`, ready for
    /// `cairo::Context::set_source_rgba`.
    pub(super) fn rgba_f64(self) -> (f64, f64, f64, f64) {
        (self.r as f64, self.g as f64, self.b as f64, self.a as f64)
    }

    /// CSS spelling: `#rrggbb` when opaque, otherwise `rgba(r, g, b, a)`.
    pub(super) fn css(self) -> String {
        let r = (self.r * 255.0).round() as u8;
        let g = (self.g * 255.0).round() as u8;
        let b = (self.b * 255.0).round() as u8;
        if self.a >= 1.0 {
            format!("#{r:02x}{g:02x}{b:02x}")
        } else {
            format!("rgba({r}, {g}, {b}, {})", self.a)
        }
    }
}

/// Every colour bnksound paints, named by role. `Copy` so it threads
/// through the widget-build chain by value without ceremony.
#[derive(Clone, Copy)]
pub(super) struct Palette {
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
    // Accent + status. The brand pink (`accent`) is only ever the
    // active/selected state on custom widgets, never structural chrome.
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
    // Contrast washes, named by alpha percentage. `dim_grid` is the faintest,
    // shared by the slider trough (CSS) and the meter's unlit grid (cairo).
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
    // Hand-drawn level meter (cairo). Deliberately distinct from the
    // slider fill: these read "how loud", not "what gain".
    pub meter_red: Color,
    pub meter_amber: Color,
    pub meter_green: Color,
    pub meter_neutral: Color,
    // Unity-gain reference notch on the slider trough (cairo).
    pub unity_notch: Color,
}

impl Palette {
    /// bnklab-native's dark palette (the only theme today).
    pub(super) fn dark() -> Self {
        Self {
            bg: Color::hex(0x1f, 0x21, 0x24),
            surface: Color::hex(0x26, 0x29, 0x2e),
            titlebar: Color::hex(0x29, 0x2b, 0x30),
            field_bg: Color::hex(0x2c, 0x2f, 0x34),
            text: Color::hex(0xec, 0xec, 0xec),
            text_subtle: Color::hex(0xaa, 0xaa, 0xaa),
            text_muted: Color::hex(0x88, 0x88, 0x88),
            text_modal_body: Color::hex(0xcc, 0xcc, 0xcc),
            text_idle: Color::hex(0x6b, 0x69, 0x69),
            on_filled: Color::hex(0xff, 0xff, 0xff),
            accent: Color::hex(0xff, 0x00, 0xaa),
            warning: Color::hex(0xff, 0x70, 0x43),
            volume_ok: Color::hex(0x66, 0xbb, 0x6a),
            filter: Color::hex(0xe5, 0xa9, 0x21),
            filter_hover: Color::hex(0xd6, 0x9e, 0x2e),
            border: Color::hex(0x33, 0x33, 0x33),
            cta_bg: Color::hex(0x31, 0x38, 0x44),
            cta_bg_hover: Color::hex(0x3d, 0x44, 0x50),
            danger_bg: Color::hex(0x5a, 0x2d, 0x2d),
            danger_bg_hover: Color::hex(0x6e, 0x36, 0x36),
            scale_fill: Color::hex(0x4c, 0xaf, 0x50),
            scale_fill_warning: Color::hex(0xf4, 0x51, 0x1e),
            dim_grid: Color::hexa(0xff, 0xff, 0xff, 0.03),
            wash_4: Color::hexa(0xff, 0xff, 0xff, 0.04),
            wash_5: Color::hexa(0xff, 0xff, 0xff, 0.05),
            wash_6: Color::hexa(0xff, 0xff, 0xff, 0.06),
            wash_8: Color::hexa(0xff, 0xff, 0xff, 0.08),
            wash_10: Color::hexa(0xff, 0xff, 0xff, 0.10),
            wash_18: Color::hexa(0xff, 0xff, 0xff, 0.18),
            wash_20: Color::hexa(0xff, 0xff, 0xff, 0.20),
            wash_30: Color::hexa(0xff, 0xff, 0xff, 0.30),
            backdrop: Color::hexa(0x00, 0x00, 0x00, 0.5),
            shadow_strong: Color::hexa(0x00, 0x00, 0x00, 0.25),
            shadow_soft: Color::hexa(0x00, 0x00, 0x00, 0.18),
            meter_red: Color::rgbaf(1.0, 0.30, 0.30, 0.95),
            meter_amber: Color::rgbaf(1.0, 0.78, 0.30, 0.95),
            meter_green: Color::rgbaf(0.30, 0.80, 0.45, 0.95),
            meter_neutral: Color::rgbaf(0.55, 0.65, 0.78, 0.85),
            unity_notch: Color::rgbaf(0.6, 0.6, 0.6, 0.55),
        }
    }

    /// The `@define-color` block mapping every CSS colour name to this
    /// palette's value, prepended to [`super::css`]'s template. The
    /// cairo-only entries (`meter_*`, `unity_notch`) are intentionally absent.
    pub(super) fn define_colors(&self) -> String {
        let entries = [
            ("bnk_bg", self.bg),
            ("bnk_surface", self.surface),
            ("bnk_titlebar", self.titlebar),
            ("bnk_field_bg", self.field_bg),
            ("bnk_text", self.text),
            ("bnk_text_subtle", self.text_subtle),
            ("bnk_text_muted", self.text_muted),
            ("bnk_text_modal_body", self.text_modal_body),
            ("bnk_text_idle", self.text_idle),
            ("bnk_on_filled", self.on_filled),
            ("bnk_accent", self.accent),
            ("bnk_warning", self.warning),
            ("bnk_volume_ok", self.volume_ok),
            ("bnk_filter", self.filter),
            ("bnk_filter_hover", self.filter_hover),
            ("bnk_border", self.border),
            ("bnk_cta_bg", self.cta_bg),
            ("bnk_cta_bg_hover", self.cta_bg_hover),
            ("bnk_danger_bg", self.danger_bg),
            ("bnk_danger_bg_hover", self.danger_bg_hover),
            ("bnk_scale_fill", self.scale_fill),
            ("bnk_scale_fill_warning", self.scale_fill_warning),
            ("bnk_dim_grid", self.dim_grid),
            ("bnk_wash_4", self.wash_4),
            ("bnk_wash_5", self.wash_5),
            ("bnk_wash_6", self.wash_6),
            ("bnk_wash_8", self.wash_8),
            ("bnk_wash_10", self.wash_10),
            ("bnk_wash_18", self.wash_18),
            ("bnk_wash_20", self.wash_20),
            ("bnk_wash_30", self.wash_30),
            ("bnk_backdrop", self.backdrop),
            ("bnk_shadow_strong", self.shadow_strong),
            ("bnk_shadow_soft", self.shadow_soft),
        ];
        let mut out = String::new();
        for (name, color) in entries {
            // Writing into a String is infallible.
            let _ = writeln!(out, "@define-color {name} {};", color.css());
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn opaque_colors_emit_hex() {
        assert_eq!(Color::hex(0xec, 0xec, 0xec).css(), "#ececec");
        assert_eq!(Color::hex(0xff, 0x00, 0xaa).css(), "#ff00aa");
        assert_eq!(Color::hex(0x1f, 0x21, 0x24).css(), "#1f2124");
    }

    #[test]
    fn translucent_colors_emit_rgba() {
        assert_eq!(
            Color::hexa(0xff, 0xff, 0xff, 0.08).css(),
            "rgba(255, 255, 255, 0.08)"
        );
        assert_eq!(
            Color::hexa(0x00, 0x00, 0x00, 0.5).css(),
            "rgba(0, 0, 0, 0.5)"
        );
        assert_eq!(
            Color::hexa(0xff, 0xff, 0xff, 0.03).css(),
            "rgba(255, 255, 255, 0.03)"
        );
    }

    #[test]
    fn hex_round_trips_losslessly() {
        // Every 8-bit channel must survive the f32 store + round-on-emit.
        for byte in 0u8..=255 {
            assert_eq!(
                Color::hex(byte, byte, byte).css(),
                format!("#{byte:02x}{byte:02x}{byte:02x}")
            );
        }
    }

    #[test]
    fn cairo_tints_stay_exact() {
        // Meter tints are read straight back by cairo, no 8-bit round-trip.
        assert_eq!(
            Color::rgbaf(0.30, 0.80, 0.45, 0.95).rgba_f64(),
            (
                0.30_f32 as f64,
                0.80_f32 as f64,
                0.45_f32 as f64,
                0.95_f32 as f64
            )
        );
    }

    #[test]
    fn define_colors_defines_before_reference() {
        // Every line is a well-formed @define-color statement.
        let css = Palette::dark().define_colors();
        let lines: Vec<&str> = css.lines().collect();
        assert!(lines.len() >= 30);
        for line in lines {
            assert!(line.starts_with("@define-color bnk_"));
            assert!(line.ends_with(';'));
        }
    }
}

//! Window geometry value type: last normal-state size plus maximized flag, so
//! a relaunch lands on the same shape. Position isn't tracked because xdg-shell
//! doesn't let Wayland clients set it. Persistence lives in
//! [`crate::store`]; this module is just the type plus clamp logic.

pub const DEFAULT_WIDTH: u32 = 560;
pub const DEFAULT_HEIGHT: u32 = 720;

/// Clamp bounds: a corrupt 0x0 or huge saved size would render the window
/// invisible, so clamp on decode to keep the user from being locked out.
const MIN_DIM: u32 = 200;
const MAX_DIM: u32 = 8192;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Geometry {
    pub width: u32,
    pub height: u32,
    pub maximized: bool,
}

impl Default for Geometry {
    fn default() -> Self {
        Self {
            width: DEFAULT_WIDTH,
            height: DEFAULT_HEIGHT,
            maximized: false,
        }
    }
}

impl Geometry {
    /// Build from raw decoded values, clamping dimensions into a sane range.
    pub fn clamped(width: u32, height: u32, maximized: bool) -> Self {
        Self {
            width: width.clamp(MIN_DIM, MAX_DIM),
            height: height.clamp(MIN_DIM, MAX_DIM),
            maximized,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn clamped_pins_undersized_dims_to_min() {
        let g = Geometry::clamped(10, 5, false);
        assert_eq!(g.width, MIN_DIM);
        assert_eq!(g.height, MIN_DIM);
        assert!(!g.maximized);
    }

    #[test]
    fn clamped_pins_oversized_dims_to_max() {
        let g = Geometry::clamped(999_999, 999_999, true);
        assert_eq!(g.width, MAX_DIM);
        assert_eq!(g.height, MAX_DIM);
        assert!(g.maximized);
    }

    #[test]
    fn clamped_leaves_in_range_dims_untouched() {
        let g = Geometry::clamped(1024, 768, false);
        assert_eq!(g.width, 1024);
        assert_eq!(g.height, 768);
    }
}

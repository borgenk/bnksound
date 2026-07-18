//! The portable platform layer: the parts that are not about mixing audio at
//! all, but about talking to the machine.
//!
//! - **Wayland**: the wire codec (`wire`), the connection (`conn`), the
//!   hand-transcribed protocol constants (`protocol`), and shared-memory
//!   buffers (`shm`).
//! - **Input**: keyboard translation via libxkbcommon (`xkb`).
//! - **Font**: FreeType rasterization (`freetype`), the colour-emoji face
//!   (`emoji`), HarfBuzz shaping (`shape`), UAX #29 grapheme segmentation
//!   (`grapheme`), system font discovery for characters our own font list has
//!   no answer for (`fontconfig`), and ARGB pixel math (`pixel`).
//! - **Foundation**: the eventfd and poll surface the loops wait on (`sys`),
//!   and fixed-capacity text for the frame path (`arena`).
//!
//! The layer is a leaf, which is what lets it be lifted out whole. A module
//! here may depend only on other `platform` modules and `std`, never on the
//! application above it. Rust does not enforce that inside one crate, so the
//! test at the bottom does.

pub mod arena;
pub mod conn;
pub mod emoji;
pub mod fontconfig;
pub mod freetype;
pub mod grapheme;
pub mod pixel;
pub mod protocol;
pub mod shape;
pub mod shm;
pub mod sys;
pub mod wire;
pub mod xkb;

#[cfg(test)]
mod tests {
    /// The layer must stay a clean leaf so it can be lifted or vendored without
    /// untangling dependencies: a module here may name only `crate::platform::*`
    /// and `std`, never `crate::ui`, `crate::render`, or anything else above it.
    #[test]
    fn platform_only_depends_on_itself() {
        let dir = concat!(env!("CARGO_MANIFEST_DIR"), "/src/platform");
        let mut offenders = Vec::new();
        for entry in std::fs::read_dir(dir).expect("read src/platform") {
            let path = entry.expect("dir entry").path();
            if path.extension().and_then(|e| e.to_str()) != Some("rs") {
                continue;
            }
            let src = std::fs::read_to_string(&path).expect("read platform source");
            let name = path.file_name().unwrap_or_default().to_string_lossy();
            for (line_no, line) in src.lines().enumerate() {
                // Comments may cross-reference any module freely.
                if line.trim_start().starts_with("//") {
                    continue;
                }
                for (idx, _) in line.match_indices("crate::") {
                    let after = &line[idx + "crate::".len()..];
                    let module: String = after
                        .chars()
                        .take_while(|c| c.is_alphanumeric() || *c == '_')
                        .collect();
                    if !module.is_empty() && module != "platform" {
                        offenders.push(format!("{name}:{}: crate::{module}", line_no + 1));
                    }
                }
            }
        }
        assert!(
            offenders.is_empty(),
            "platform modules must not reach outside the layer:\n{}",
            offenders.join("\n")
        );
    }
}

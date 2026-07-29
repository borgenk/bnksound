//! Development tooling, off the mixer's runtime path.
//!
//! The performance gate (`perf`) and the counting allocator it reads (`alloc`,
//! installed only under the `perf-alloc` feature), the Unicode table generator
//! (`gen_tables`), a frame renderer for looking at the UI without a compositor
//! (`frame`), a bounded window run that reports what a compositor offered
//! (`probe`), and the fixtures they all draw (`scene`).
//!
//! Compiled only with the `dev` feature and reached only through the flags
//! [`run`] dispatches, so the shipping binary carries none of it.

pub mod alloc;
pub mod frame;
pub mod gen_tables;
pub mod perf;
pub mod probe;
pub mod scene;

/// What a dev command reports back: anything that went wrong, in words.
pub type Error = Box<dyn std::error::Error>;
pub type Result<T> = std::result::Result<T, Error>;

/// Run the dev command `args` names, or report that it names none.
///
/// Returns `None` when the arguments are not a dev command at all, which is the
/// signal to go on and open a window.
pub fn run(args: &[String]) -> Option<Result<()>> {
    if args.iter().any(|a| a == "--perf") {
        return Some(perf::run(args));
    }
    if args.iter().any(|a| a == "--gen-tables") {
        return Some(gen_tables::run());
    }
    if args.iter().any(|a| a == "--render-frame") {
        return Some(frame::run(args));
    }
    if args.iter().any(|a| a == "--probe") {
        return Some(probe::run(args));
    }
    None
}

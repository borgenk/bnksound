//! Feature-boundary guardrail for the native/GTK migration.
//!
//! Shared bnksound code must never link the GTK stack. This test walks every
//! Rust source file under src/ and fails if one references a GTK-stack crate
//! (gtk4, gdk4, glib, gio, cairo, pango, gdk_pixbuf, graphene) outside the
//! explicit exception set below.
//!
//! GTK_ALLOWED is the migration TODO list. It starts as every file that still
//! hosts GTK today and shrinks as those files move under gtk_shell/ or shed
//! their GTK usage. The end state is a single allowed prefix, src/gtk_shell/,
//! the only place GTK may live once the migration finishes.
//!
//! The test uses only std, so it compiles and runs in both the native
//! (no-default-features) and GTK build matrices.

use std::path::{Path, PathBuf};

/// Source paths still permitted to reference the GTK stack during migration.
/// An entry ending in `/` matches a whole subtree; otherwise it matches one
/// file. Remove entries as their GTK usage moves under gtk_shell/; the only
/// end-state entry is `src/gtk_shell/`. Do not add files: new shared code is
/// GTK-free by construction.
const GTK_ALLOWED: &[&str] = &["src/gtk_shell/", "src/bin/bnksound-gtk.rs"];

/// Substrings that appear only in genuine GTK-stack usage: crate names, path
/// segments, and imports. Matched case-sensitively so prose comments that
/// mention "GTK" in uppercase do not trip the check.
const FORBIDDEN: &[&str] = &[
    "gtk4",
    "gdk4",
    "gdk_pixbuf",
    "graphene::",
    "gdk::",
    "gtk::",
    "glib::",
    "gio::",
    "cairo::",
    "pango::",
    "pangocairo",
    "use glib",
    "use gio",
    "use gtk",
    "use gdk",
    "use cairo",
    "use pango",
];

#[test]
fn shared_source_has_no_gtk_stack_imports() {
    let manifest = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let src = manifest.join("src");

    let mut files = Vec::new();
    collect_rs_files(&src, &mut files);
    assert!(!files.is_empty(), "found no Rust sources under {src:?}");

    let mut violations = Vec::new();
    for file in &files {
        let rel = rel_path(&manifest, file);
        if is_allowed(&rel) {
            continue;
        }
        let contents = std::fs::read_to_string(file)
            .unwrap_or_else(|e| panic!("failed to read {}: {e}", file.display()));
        for (idx, line) in contents.lines().enumerate() {
            // The guardrail is about linkage, not prose: a doc comment may name
            // gtk::HeaderBar while the module imports nothing. Comment cleanup
            // is tracked separately, so skip comment lines here.
            if is_comment_line(line) {
                continue;
            }
            for pat in FORBIDDEN {
                if line.contains(pat) {
                    violations.push(format!(
                        "{rel}:{}: matches `{pat}` -> {}",
                        idx + 1,
                        line.trim()
                    ));
                }
            }
        }
    }

    assert!(
        violations.is_empty(),
        "shared source referenced the GTK stack:\n{}\n\n\
         Shared modules must not import gtk4, gdk4, glib, gio, cairo, pango, \
         gdk_pixbuf, or graphene. Move the GTK usage under src/gtk_shell/, or if \
         this file is a known migration holdout, add it to GTK_ALLOWED in \
         tests/feature_boundary.rs (and plan to remove it).",
        violations.join("\n"),
    );
}

/// Recursively collect every `.rs` file under `dir` into `out`.
fn collect_rs_files(dir: &Path, out: &mut Vec<PathBuf>) {
    let entries = std::fs::read_dir(dir)
        .unwrap_or_else(|e| panic!("failed to read dir {}: {e}", dir.display()));
    for entry in entries {
        let entry = entry.expect("failed to read dir entry");
        let path = entry.path();
        if path.is_dir() {
            collect_rs_files(&path, out);
        } else if path.extension().is_some_and(|ext| ext == "rs") {
            out.push(path);
        }
    }
}

/// Whether a line is a line comment, doc comment, or block-comment
/// continuation. Trailing comments on code lines are left in scope so a stray
/// `let _ = gtk::foo(); // note` still trips the check.
fn is_comment_line(line: &str) -> bool {
    let t = line.trim_start();
    t.starts_with("//") || t.starts_with("/*") || t.starts_with('*')
}

/// Repo-relative path with forward slashes, e.g. `src/ui/mod.rs`.
fn rel_path(manifest: &Path, file: &Path) -> String {
    file.strip_prefix(manifest)
        .unwrap_or(file)
        .to_string_lossy()
        .replace('\\', "/")
}

/// Whether `rel` is on the GTK exception list: an exact file match, or under a
/// directory entry ending in `/`.
fn is_allowed(rel: &str) -> bool {
    GTK_ALLOWED.iter().any(|allowed| {
        if let Some(dir) = allowed.strip_suffix('/') {
            rel.starts_with(dir) && rel.as_bytes().get(dir.len()) == Some(&b'/')
        } else {
            rel == *allowed
        }
    })
}

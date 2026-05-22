//! Clean-architecture assertion: nothing in `src/domain/**` may import
//! from `crate::adapters` or `crate::cli`.
//!
//! ADR-1 commits to hexagonal inward-dependency discipline
//! (`domain → ports → adapters → cli`) but a single-crate project
//! cannot fail-to-compile on an inward `use` — the discipline is
//! editorial. This test gives that discipline a build-break by walking
//! `src/domain/**/*.rs` and failing if any non-comment line imports an
//! outward layer — `crate::adapters` or `crate::cli` (▸AUDIT CAO-R2
//! carried over from the impl-plan).
//!
//! Kept deliberately simple — no full Rust parser, no regex crate;
//! line-level scanning with comment-stripping is enough to catch every
//! realistic violation. If a future contributor needs richer enforcement
//! (e.g. detecting `super::super::adapters`), upgrade to `syn`.

use std::fs;
use std::path::{Path, PathBuf};

/// Imports forbidden in the `domain` layer. ADR-1's inward-dependency
/// discipline forbids `domain` reaching outward to *either* `adapters`
/// or `cli`; the guard checks both.
const FORBIDDEN: [&str; 2] = ["use crate::adapters", "use crate::cli"];

fn collect_rs_files(dir: &Path, out: &mut Vec<PathBuf>) {
    for entry in fs::read_dir(dir).expect("read_dir on src/domain succeeded") {
        let entry = entry.expect("dir entry");
        let path = entry.path();
        if path.is_dir() {
            collect_rs_files(&path, out);
        } else if path.extension().and_then(|s| s.to_str()) == Some("rs") {
            out.push(path);
        }
    }
}

/// Strip `//` line comments so a doc-comment referencing
/// `use crate::adapters` (e.g. "ports/ never import adapters") does
/// not trigger a false positive.
fn strip_line_comment(line: &str) -> &str {
    match line.find("//") {
        Some(idx) => &line[..idx],
        None => line,
    }
}

#[test]
fn domain_imports_nothing_outward() {
    let domain_root = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("src")
        .join("domain");
    let mut files = Vec::new();
    collect_rs_files(&domain_root, &mut files);

    assert!(
        !files.is_empty(),
        "src/domain/ contained no .rs files — test fixture broken"
    );

    let mut violations = Vec::new();
    for file in &files {
        let content = fs::read_to_string(file).expect("read domain file");
        for (lineno, raw) in content.lines().enumerate() {
            let code = strip_line_comment(raw);
            for forbidden in FORBIDDEN {
                if code.contains(forbidden) {
                    violations.push(format!(
                        "{}:{}: forbidden `{forbidden}` in domain layer",
                        file.display(),
                        lineno + 1
                    ));
                }
            }
        }
    }

    assert!(
        violations.is_empty(),
        "ADR-1 clean-architecture violation:\n{}",
        violations.join("\n")
    );
}

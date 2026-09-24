//! Mechanical enforcement of the source-file size limit.
//!
//! Clippy has no file-length lint, so it lives here: the check then runs with
//! the suite everybody already runs, travels with the repository, and names the
//! offending files when it fails.

use std::path::{Path, PathBuf};

/// Files longer than this fail the build.
const HARD_LIMIT: usize = 1000;

/// Files longer than this are named, so the seam gets found while splitting is
/// still cheap.
const SOFT_LIMIT: usize = 600;

/// Every `.rs` file in the workspace's own source, excluding build output.
fn source_files() -> Vec<PathBuf> {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let mut roots = vec![root.join("src"), root.join("tests")];

    if let Ok(crates) = std::fs::read_dir(root.join("crates")) {
        for entry in crates.flatten() {
            roots.push(entry.path().join("src"));
        }
    }

    let mut found = Vec::new();
    while let Some(dir) = roots.pop() {
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                roots.push(path);
            } else if path.extension().is_some_and(|e| e == "rs") {
                found.push(path);
            }
        }
    }
    found
}

#[test]
fn no_source_file_exceeds_the_size_limit() {
    let files = source_files();
    assert!(
        !files.is_empty(),
        "found no source files; the walker is looking in the wrong place"
    );

    let mut over = Vec::new();
    for path in files {
        let Ok(text) = std::fs::read_to_string(&path) else {
            continue;
        };
        let lines = text.lines().count();
        let name = path.display();

        if lines > HARD_LIMIT {
            over.push(format!("{name} — {lines} lines (limit {HARD_LIMIT})"));
        } else if lines > SOFT_LIMIT {
            eprintln!("note: {name} is {lines} lines, approaching the {HARD_LIMIT} limit");
        }
    }

    assert!(
        over.is_empty(),
        "files over the size limit:\n{}",
        over.join("\n")
    );
}

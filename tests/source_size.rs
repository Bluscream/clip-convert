//! Mechanical enforcement of the source size limits.
//!
//! Clippy has no file-length lint, and `too_many_lines` counts only
//! statements, so both live here: the checks then run with the suite everybody
//! already runs, travel with the repository, and name what failed.

use std::path::{Path, PathBuf};

/// Files longer than this fail the build.
const HARD_LIMIT: usize = 1000;

/// Files longer than this are named, so the seam gets found while splitting is
/// still cheap.
const SOFT_LIMIT: usize = 600;

/// Functions longer than this fail the build.
///
/// A function that does not fit on a screen cannot be read as one thought,
/// and the limit is what forces the seam to be found rather than deferred.
const FUNCTION_LIMIT: usize = 100;

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

/// Blanks out everything whose braces are not code: line comments, and string
/// and character literals. Crude, but it only has to preserve brace balance.
fn without_literals(line: &str) -> String {
    let mut out = String::with_capacity(line.len());
    let mut chars = line.chars().peekable();
    let mut quote: Option<char> = None;

    while let Some(c) = chars.next() {
        match quote {
            Some(open) => {
                if c == '\\' {
                    chars.next();
                } else if c == open {
                    quote = None;
                }
            }
            None => match c {
                '"' | '\'' => quote = Some(c),
                '/' if chars.peek() == Some(&'/') => break,
                _ => out.push(c),
            },
        }
    }
    out
}

/// Every function in `text`, as (line it starts on, how many lines it spans).
///
/// A signature may span several lines, so the body is taken to start at the
/// first `{` at or after the `fn`, and to end where brace depth returns to
/// zero. Nested functions and closures are counted within their parent, which
/// is the right answer: they are part of what has to be read.
fn functions(text: &str) -> Vec<(usize, usize)> {
    let mut found = Vec::new();
    let mut start = 0;
    let mut depth = 0i32;
    let mut pending = false;

    for (index, raw) in text.lines().enumerate() {
        let line = without_literals(raw);

        if depth == 0 && !pending && declares_a_function(&line) {
            pending = true;
            start = index;
        }
        if !pending && depth == 0 {
            continue;
        }

        let opens = i32::try_from(line.matches('{').count()).unwrap_or(0);
        let closes = i32::try_from(line.matches('}').count()).unwrap_or(0);
        if pending && opens > 0 {
            pending = false;
        }
        depth += opens - closes;

        if !pending && depth <= 0 {
            found.push((start + 1, index - start + 1));
            depth = 0;
        }
    }
    found
}

/// Whether a line declares a function, rather than merely mentioning one.
fn declares_a_function(line: &str) -> bool {
    line.match_indices("fn ").any(|(at, _)| {
        // `transfn foo` and `r#fn` are not declarations; a bare `fn` is.
        !line[..at]
            .chars()
            .next_back()
            .is_some_and(|c| c.is_alphanumeric() || c == '_' || c == '#')
    })
}

#[test]
fn no_function_exceeds_the_size_limit() {
    let mut over = Vec::new();

    for path in source_files() {
        let Ok(text) = std::fs::read_to_string(&path) else {
            continue;
        };
        for (line, length) in functions(&text) {
            if length > FUNCTION_LIMIT {
                over.push(format!(
                    "{}:{line} — {length} lines (limit {FUNCTION_LIMIT})",
                    path.display()
                ));
            }
        }
    }

    assert!(
        over.is_empty(),
        "functions over the size limit:\n{}",
        over.join("\n")
    );
}

#[cfg(test)]
mod tests {
    use super::{functions, without_literals};

    #[test]
    fn a_function_spans_from_its_signature_to_its_closing_brace() {
        let text = "fn one() {\n    let x = 1;\n}\n\nfn two() {\n}\n";
        assert_eq!(functions(text), [(1, 3), (5, 2)]);
    }

    #[test]
    fn a_signature_split_over_lines_still_starts_at_the_fn() {
        let text = "fn wide(\n    a: u32,\n) -> u32 {\n    a\n}\n";
        assert_eq!(functions(text), [(1, 5)]);
    }

    #[test]
    fn a_nested_item_is_counted_within_its_parent() {
        let text = "fn outer() {\n    fn inner() {\n    }\n}\n";
        assert_eq!(functions(text), [(1, 4)]);
    }

    #[test]
    fn braces_in_strings_and_comments_do_not_shift_the_count() {
        let text = "fn one() {\n    let s = \"{{{\"; // }}}\n}\n";
        assert_eq!(functions(text), [(1, 3)]);
        assert_eq!(without_literals("let s = \"a}b\"; // }"), "let s = ; ");
    }

    #[test]
    fn a_word_ending_in_fn_is_not_a_declaration() {
        // `transfn` and a doc comment mentioning "fn " must not open a function.
        let text = "// see fn one\nstruct S;\n";
        assert_eq!(functions(text), []);
    }
}

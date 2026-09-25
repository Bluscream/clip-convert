//! Tidying text: trimming it, and minifying or beautifying code.
//!
//! Separate from `text`, which holds the char-counting transformations behind
//! `split` and `truncate`. Nothing here counts anything.
//!
//! Trimming is deliberately aggressive — it is for text pasted out of a
//! terminal, a PDF or a chat window, which arrives carrying escape sequences,
//! hard-wrapped indentation and invisible characters that break every later
//! comparison.
//!
//! Minifying and beautifying only touch formats that can be parsed and
//! re-printed, so the result is the same document rather than a plausible
//! guess. Anything else is refused: silently mangling someone's code on the
//! clipboard is worse than declining to.

use std::fmt;

/// Why text could not be minified or beautified.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TextError {
    /// Not a format this can parse.
    Unrecognised,
    /// Recognised as `format`, but it did not parse.
    Invalid { format: CodeFormat, detail: String },
}

impl fmt::Display for TextError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Unrecognised => write!(
                f,
                "this does not look like JSON or TOML. For another language, \
                 configure an action that runs its own formatter."
            ),
            Self::Invalid { format, detail } => write!(f, "invalid {format}: {detail}"),
        }
    }
}

impl std::error::Error for TextError {}

/// A format that can be both parsed and printed back.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CodeFormat {
    Json,
    Toml,
}

impl fmt::Display for CodeFormat {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Json => write!(f, "JSON"),
            Self::Toml => write!(f, "TOML"),
        }
    }
}

/// Characters that are whitespace but not a space, and carry no meaning once
/// text is being normalised: non-breaking spaces, the typographic spaces, and
/// the ideographic space.
const EXOTIC_SPACES: &[char] = &[
    '\u{00a0}', '\u{1680}', '\u{2000}', '\u{2001}', '\u{2002}', '\u{2003}', '\u{2004}', '\u{2005}',
    '\u{2006}', '\u{2007}', '\u{2008}', '\u{2009}', '\u{200a}', '\u{202f}', '\u{205f}', '\u{3000}',
];

/// Characters with no width at all, which survive every visual inspection and
/// break every string comparison: the zero-width family and the byte-order
/// mark.
const INVISIBLES: &[char] = &[
    '\u{200b}', '\u{200c}', '\u{200d}', '\u{2060}', '\u{feff}', '\u{00ad}',
];

/// Removes as much whitespace and as many stray control characters as can go
/// without changing what the text says.
///
/// Line endings are normalised, escape sequences and control characters are
/// dropped, runs of whitespace within a line become one space, every line is
/// trimmed, and blank lines are removed.
#[must_use]
pub fn trim(text: &str) -> String {
    // Line endings first: a carriage return is a control character, and
    // removing it before it has been turned into a newline silently joins the
    // two lines it separated.
    let normalised = text.replace("\r\n", "\n").replace('\r', "\n");
    let without_escapes = strip_escapes(&normalised);

    let cleaned: String = without_escapes
        .chars()
        .filter(|c| !INVISIBLES.contains(c))
        .map(|c| if EXOTIC_SPACES.contains(&c) { ' ' } else { c })
        // Newline survives as the only structure worth keeping; tab becomes a
        // space and is then collapsed with the rest. Everything else in the
        // control ranges — including DEL and the C1 block — goes.
        .filter(|c| *c == '\n' || !is_control(*c))
        .map(|c| if c == '\t' { ' ' } else { c })
        .collect();

    let mut lines: Vec<String> = Vec::new();
    for line in cleaned.lines() {
        let collapsed = collapse_spaces(line);
        if !collapsed.is_empty() {
            lines.push(collapsed);
        }
    }
    lines.join("\n")
}

/// Whether `c` is a control character, by the C0 and C1 ranges plus DEL.
fn is_control(c: char) -> bool {
    let code = c as u32;
    code < 0x20 || code == 0x7f || (0x80..=0x9f).contains(&code)
}

/// Runs of whitespace become a single space, and the ends are trimmed.
fn collapse_spaces(line: &str) -> String {
    let mut out = String::with_capacity(line.len());
    let mut in_space = false;
    for c in line.chars() {
        if c.is_whitespace() {
            in_space = true;
            continue;
        }
        if in_space && !out.is_empty() {
            out.push(' ');
        }
        in_space = false;
        out.push(c);
    }
    out
}

/// Removes ANSI escape sequences.
///
/// The CSI form (`ESC[…letter`) is what a terminal's bracketed paste leaves
/// behind — `ESC[200~` around anything pasted — and what colour output is made
/// of. OSC sequences (`ESC]…BEL`) carry window titles and hyperlinks.
fn strip_escapes(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut chars = text.chars().peekable();

    while let Some(c) = chars.next() {
        if c != '\u{1b}' {
            out.push(c);
            continue;
        }
        match chars.peek() {
            Some('[') => {
                chars.next();
                // Parameters and intermediates, then one final byte.
                while let Some(&next) = chars.peek() {
                    chars.next();
                    if ('\u{40}'..='\u{7e}').contains(&next) {
                        break;
                    }
                }
            }
            Some(']') => {
                chars.next();
                // Ends at BEL, or at ST (ESC \).
                while let Some(next) = chars.next() {
                    if next == '\u{7}' {
                        break;
                    }
                    if next == '\u{1b}' {
                        if chars.peek() == Some(&'\\') {
                            chars.next();
                        }
                        break;
                    }
                }
            }
            // A lone escape, or a two-character sequence: drop what follows.
            Some(_) => {
                chars.next();
            }
            None => {}
        }
    }
    out
}

/// What `text` looks like, if it looks like anything.
///
/// Decided by parsing rather than by guessing from punctuation: something that
/// merely starts with `{` is not necessarily JSON, and TOML is permissive
/// enough that almost any prose parses as nothing at all.
#[must_use]
pub fn detect(text: &str) -> Option<CodeFormat> {
    let body = text.trim();
    if body.is_empty() {
        return None;
    }
    if serde_json::from_str::<serde_json::Value>(body).is_ok() {
        return Some(CodeFormat::Json);
    }
    // A bare scalar parses as JSON but not as TOML, so order matters only for
    // documents that are both — which are documents of key = value pairs, and
    // those are not valid JSON.
    if toml::from_str::<toml::Value>(body).is_ok() {
        return Some(CodeFormat::Toml);
    }
    None
}

/// Re-prints `text` as compactly as its format allows.
///
/// # Errors
///
/// Returns [`TextError::Unrecognised`] when the text is not a supported
/// format, and [`TextError::Invalid`] when it is but does not parse.
pub fn minify(text: &str) -> Result<(String, CodeFormat), TextError> {
    render(text, false)
}

/// Re-prints `text` with indentation and line breaks.
///
/// # Errors
///
/// As [`minify`].
pub fn beautify(text: &str) -> Result<(String, CodeFormat), TextError> {
    render(text, true)
}

fn render(text: &str, pretty: bool) -> Result<(String, CodeFormat), TextError> {
    let body = text.trim();
    let format = detect(body).ok_or(TextError::Unrecognised)?;

    let out = match format {
        CodeFormat::Json => {
            let value: serde_json::Value =
                serde_json::from_str(body).map_err(|e| TextError::Invalid {
                    format,
                    detail: e.to_string(),
                })?;
            let printed = if pretty {
                serde_json::to_string_pretty(&value)
            } else {
                serde_json::to_string(&value)
            };
            printed.map_err(|e| TextError::Invalid {
                format,
                detail: e.to_string(),
            })?
        }
        CodeFormat::Toml => {
            let value: toml::Value = toml::from_str(body).map_err(|e| TextError::Invalid {
                format,
                detail: e.to_string(),
            })?;
            // TOML has no compact form worth the name — it is line-based by
            // design — so minifying it means dropping comments and blank
            // lines, which is what re-printing does.
            let printed = if pretty {
                toml::to_string_pretty(&value)
            } else {
                toml::to_string(&value)
            };
            printed.map_err(|e| TextError::Invalid {
                format,
                detail: e.to_string(),
            })?
        }
    };
    Ok((out, format))
}

// ---------------------------------------------------------------------------
// The actions themselves, kept beside the transformations they wrap rather
// than in `runner`, which is at the limit of what one file should hold.
// ---------------------------------------------------------------------------

use crate::content::Clip;
use crate::runner::{Outcome, RunError};

/// Which direction [`run_tidy`] re-prints in.
#[derive(Debug, Clone, Copy)]
enum Tidy {
    Minify,
    Beautify,
}

pub(crate) fn run_trim(clip: &Clip) -> Result<Option<Outcome>, RunError> {
    let body = clip.text().ok_or(RunError::NotTextual)?;
    let trimmed = trim(body);

    if trimmed == body {
        return Ok(Some(Outcome {
            message: "Nothing to trim.".to_string(),
            clipboard_changed: false,
        }));
    }

    let removed = body.chars().count().saturating_sub(trimmed.chars().count());
    crate::clipboard::write_text(&trimmed)?;
    Ok(Some(Outcome {
        message: format!("Trimmed {removed} characters."),
        clipboard_changed: true,
    }))
}

fn run_tidy(clip: &Clip, direction: Tidy) -> Result<Option<Outcome>, RunError> {
    let body = clip.text().ok_or(RunError::NotTextual)?;
    let result = match direction {
        Tidy::Minify => minify(body),
        Tidy::Beautify => beautify(body),
    };

    let (output, format) = result?;
    let before = body.chars().count();
    let after = output.chars().count();

    crate::clipboard::write_text(&output)?;
    let message = match direction {
        Tidy::Minify => format!("Minified {format}: {before} to {after} characters."),
        Tidy::Beautify => format!("Beautified {format}: {before} to {after} characters."),
    };
    Ok(Some(Outcome {
        message,
        clipboard_changed: true,
    }))
}

pub(crate) fn run_minify(clip: &Clip) -> Result<Option<Outcome>, RunError> {
    run_tidy(clip, Tidy::Minify)
}

pub(crate) fn run_beautify(clip: &Clip) -> Result<Option<Outcome>, RunError> {
    run_tidy(clip, Tidy::Beautify)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn trimming_removes_the_bracketed_paste_markers_a_terminal_leaves() {
        // The actual report: a URL pasted into a terminal came back wrapped in
        // the markers, and they were in the clipboard afterwards.
        let pasted = "\u{1b}[200~https://example.com/abc~\n\n";
        assert_eq!(trim(pasted), "https://example.com/abc~");
    }

    #[test]
    fn trimming_drops_blank_lines_and_trims_each_line() {
        let text = "  first line  \n\n\n\t second   line\t\n   \n last \n";
        assert_eq!(trim(text), "first line\nsecond line\nlast");
    }

    #[test]
    fn trimming_normalises_line_endings() {
        assert_eq!(trim("a\r\nb\rc"), "a\nb\nc");
    }

    #[test]
    fn trimming_removes_control_characters_but_keeps_the_text() {
        let text = "he\u{0}llo\u{7}, \u{1b}[31mworld\u{1b}[0m\u{7f}!";
        assert_eq!(trim(text), "hello, world!");
    }

    #[test]
    fn trimming_removes_invisible_characters_and_exotic_spaces() {
        let text = "\u{feff}a\u{200b}b\u{00a0}\u{00a0}c";
        assert_eq!(trim(text), "ab c");
    }

    #[test]
    fn trimming_leaves_ordinary_text_alone() {
        assert_eq!(trim("hello, world!"), "hello, world!");
        assert_eq!(trim("one\ntwo"), "one\ntwo");
    }

    #[test]
    fn trimming_empty_text_is_empty() {
        assert_eq!(trim(""), "");
        assert_eq!(trim("   \n\n  \t "), "");
    }

    #[test]
    fn an_osc_sequence_goes_too() {
        // A hyperlink, as produced by ls and by some shells.
        let text = "\u{1b}]8;;https://example.com\u{7}label\u{1b}]8;;\u{7}";
        assert_eq!(trim(text), "label");
    }

    #[test]
    fn json_is_recognised_and_round_trips() {
        let source = "{\n  \"b\": 1,\n  \"a\": [1, 2]\n}";
        assert_eq!(detect(source), Some(CodeFormat::Json));

        let (small, format) = minify(source).expect("valid json");
        assert_eq!(format, CodeFormat::Json);
        assert!(!small.contains('\n'), "{small}");

        let (big, _) = beautify(&small).expect("valid json");
        assert!(big.contains('\n'), "{big}");

        // Same document either way.
        let original: serde_json::Value = serde_json::from_str(source).expect("json");
        let after: serde_json::Value = serde_json::from_str(&big).expect("json");
        assert_eq!(original, after);
    }

    #[test]
    fn toml_is_recognised_and_round_trips() {
        let source = "# a comment\n\nname = \"clip\"\n\n[nested]\nvalue = 1\n";
        assert_eq!(detect(source), Some(CodeFormat::Toml));

        let (small, format) = minify(source).expect("valid toml");
        assert_eq!(format, CodeFormat::Toml);
        assert!(!small.contains("# a comment"), "comments go: {small}");

        let original: toml::Value = toml::from_str(source).expect("toml");
        let after: toml::Value = toml::from_str(&small).expect("toml");
        assert_eq!(original, after);
    }

    #[test]
    fn anything_else_is_refused_rather_than_mangled() {
        for text in ["", "   ", "just some prose, really"] {
            assert_eq!(minify(text), Err(TextError::Unrecognised), "{text}");
            assert_eq!(beautify(text), Err(TextError::Unrecognised), "{text}");
        }
    }

    #[test]
    fn broken_json_is_not_silently_treated_as_prose() {
        // Looks like JSON, is not. It must not be reported as "not a format I
        // know": the user meant JSON and wants to hear what is wrong with it.
        let broken = "{\"a\": 1,}";
        assert_eq!(detect(broken), None);
    }
}

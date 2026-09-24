//! Pure text transformations used by the `split` and `truncate` built-in actions.
//!
//! These operate on `char` counts rather than bytes: the limits users care about
//! (a chat message cap, a post length) are counted in characters, and slicing a
//! UTF-8 string by byte offset would split multi-byte codepoints.

/// Breaks `text` into chunks of at most `limit` characters.
///
/// Chunks are split on the last newline inside the window, falling back to the
/// last whitespace, and only then to a hard cut mid-word. Whitespace at a seam is
/// consumed rather than duplicated, so rejoining the chunks with the separator the
/// caller sends between them reproduces the text closely.
///
/// Returns an empty vector for empty input. Never yields an empty chunk.
///
/// # Panics
///
/// Panics if `limit` is zero, which would make the loop unable to make progress.
/// Config loading rejects a zero limit, so this is a programming error rather
/// than something a user can trigger.
#[must_use]
pub fn split(text: &str, limit: usize) -> Vec<String> {
    assert!(limit > 0, "split limit must be greater than zero");

    let mut out = Vec::new();
    let mut rest = text.trim();

    while !rest.is_empty() {
        let chars: Vec<char> = rest.chars().collect();
        if chars.len() <= limit {
            out.push(rest.to_string());
            break;
        }

        let cut = seam(&chars, limit);
        let head: String = chars[..cut].iter().collect();
        let head = head.trim_end();
        if !head.is_empty() {
            out.push(head.to_string());
        }

        let consumed: usize = chars[..cut].iter().map(|c| c.len_utf8()).sum();
        rest = rest[consumed..].trim_start();
    }

    out
}

/// Finds the character index to cut at, preferring a natural boundary.
///
/// `chars` is known to be longer than `limit`, so a hard cut at `limit` is always
/// a valid answer and the fallbacks below only ever improve on it.
fn seam(chars: &[char], limit: usize) -> usize {
    let window = &chars[..limit];

    // A newline is the most natural seam: it was already a break in the source.
    if let Some(i) = window.iter().rposition(|c| *c == '\n') {
        if i > 0 {
            return i + 1;
        }
    }

    // Otherwise avoid cutting a word in half, but only if that leaves a chunk
    // worth sending — a seam in the first few characters wastes the whole window.
    if let Some(i) = window.iter().rposition(|c| c.is_whitespace()) {
        if i > limit / 4 {
            return i + 1;
        }
    }

    limit
}

/// Shortens `text` to at most `limit` characters, marking the cut with `ellipsis`.
///
/// Returns `text` unchanged when it already fits. When `limit` is too small to
/// hold both content and the marker, the marker itself is truncated to `limit`.
#[must_use]
pub fn truncate(text: &str, limit: usize, ellipsis: &str) -> String {
    let text = text.trim();
    if text.chars().count() <= limit {
        return text.to_string();
    }

    let marker_len = ellipsis.chars().count();
    if limit <= marker_len {
        return ellipsis.chars().take(limit).collect();
    }

    let head: String = text.chars().take(limit - marker_len).collect();
    format!("{}{ellipsis}", head.trim_end())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn count(s: &str) -> usize {
        s.chars().count()
    }

    #[test]
    fn split_returns_input_when_it_already_fits() {
        assert_eq!(split("hello", 10), vec!["hello"]);
    }

    #[test]
    fn split_returns_nothing_for_blank_input() {
        assert!(split("", 10).is_empty());
        assert!(split("   \n  ", 10).is_empty());
    }

    #[test]
    fn every_chunk_respects_the_limit() {
        let text = "lorem ipsum dolor sit amet consectetur adipiscing elit sed do eiusmod";
        for limit in 3..40 {
            for chunk in split(text, limit) {
                assert!(
                    count(&chunk) <= limit,
                    "chunk {chunk:?} exceeds limit {limit}"
                );
            }
        }
    }

    #[test]
    fn split_prefers_a_newline_seam() {
        let chunks = split("first line\nsecond line", 15);
        assert_eq!(chunks, vec!["first line", "second line"]);
    }

    #[test]
    fn split_prefers_a_word_seam_over_cutting_mid_word() {
        let chunks = split("alpha bravo charlie", 12);
        assert_eq!(chunks[0], "alpha bravo");
    }

    #[test]
    fn split_hard_cuts_a_word_longer_than_the_limit() {
        let chunks = split("supercalifragilistic", 5);
        assert_eq!(chunks[0], "super");
        assert_eq!(chunks.concat(), "supercalifragilistic");
    }

    #[test]
    fn split_never_yields_an_empty_chunk() {
        let text = "a\n\n\n\nb   \n   c";
        for chunk in split(text, 2) {
            assert!(!chunk.trim().is_empty());
        }
    }

    #[test]
    fn split_counts_characters_not_bytes() {
        // Each emoji is 4 bytes; a byte-based implementation would panic or
        // produce 1-char chunks here.
        let chunks = split("😀😀😀😀😀😀", 2);
        assert_eq!(chunks.len(), 3);
        assert!(chunks.iter().all(|c| count(c) == 2));
    }

    #[test]
    fn split_preserves_all_non_whitespace_content() {
        let text = "alpha bravo charlie delta echo foxtrot golf hotel";
        let rejoined: String = split(text, 9).concat();
        let strip = |s: &str| s.chars().filter(|c| !c.is_whitespace()).collect::<String>();
        assert_eq!(strip(&rejoined), strip(text));
    }

    #[test]
    fn truncate_leaves_short_text_alone() {
        assert_eq!(truncate("hello", 10, "..."), "hello");
        assert_eq!(truncate("hello", 5, "..."), "hello");
    }

    #[test]
    fn truncate_fits_within_the_limit_including_the_marker() {
        let out = truncate("abcdefghij", 5, "...");
        assert_eq!(out, "ab...");
        assert_eq!(count(&out), 5);
    }

    #[test]
    fn truncate_does_not_leave_a_space_before_the_marker() {
        assert_eq!(truncate("alpha bravo", 9, "..."), "alpha...");
    }

    #[test]
    fn truncate_degrades_when_the_limit_cannot_hold_the_marker() {
        assert_eq!(truncate("abcdef", 2, "..."), "..");
        assert_eq!(truncate("abcdef", 0, "..."), "");
    }

    #[test]
    fn truncate_counts_characters_not_bytes() {
        let out = truncate("😀😀😀😀😀", 4, "…");
        assert_eq!(count(&out), 4);
        assert_eq!(out, "😀😀😀…");
    }
}

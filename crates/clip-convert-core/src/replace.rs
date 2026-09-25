//! Regular-expression search and replace over clipboard text.
//!
//! The pattern is a full regular expression and the replacement understands
//! `$1`, `${name}` and friends, which is the whole point of the action: one
//! pass over a copied block of text that a plain substring swap could not do.
//!
//! Both halves are remembered in the config file, most recent first, so a
//! pattern typed once can be picked from a list rather than retyped.

use serde::{Deserialize, Serialize};

/// A pattern and what to put in place of what it matches.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Replacement {
    pub pattern: String,
    pub replacement: String,
}

/// What the replacement did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Replaced {
    pub text: String,
    /// How many matches were substituted, so the result can be reported
    /// honestly — including when it was none.
    pub matches: usize,
}

/// Whether `pattern` is a regular expression this can use.
///
/// Exposed so a dialog can say so while it is being typed, instead of letting
/// the user press a button that then fails.
#[must_use]
pub fn is_valid(pattern: &str) -> bool {
    !pattern.is_empty() && regex::Regex::new(pattern).is_ok()
}

/// Explains why a pattern will not compile, for showing beside the field.
#[must_use]
pub fn why_invalid(pattern: &str) -> Option<String> {
    if pattern.is_empty() {
        return Some("Enter a pattern.".to_string());
    }
    match regex::Regex::new(pattern) {
        Ok(_) => None,
        // The crate's own message is several lines of caret diagram, which is
        // more than a dialog has room for; its first line names the problem.
        Err(e) => Some(
            e.to_string()
                .lines()
                .find(|line| !line.trim().is_empty())
                .unwrap_or("Not a valid regular expression.")
                .trim()
                .to_string(),
        ),
    }
}

/// Applies `replacement` to every match in `text`.
///
/// # Errors
///
/// Returns the compile error if the pattern is not a valid regular expression.
pub fn apply(text: &str, replacement: &Replacement) -> Result<Replaced, regex::Error> {
    let pattern = regex::Regex::new(&replacement.pattern)?;
    let matches = pattern.find_iter(text).count();

    Ok(Replaced {
        text: pattern
            .replace_all(text, replacement.replacement.as_str())
            .into_owned(),
        matches,
    })
}

/// Records `value` as the most recent entry of `history`.
///
/// Moving an existing entry to the front rather than adding a second copy is
/// what makes a short list stay useful: the ten things last used, not the ten
/// times something was used.
pub fn remember(history: &mut Vec<String>, value: &str, limit: usize) {
    if value.is_empty() || limit == 0 {
        return;
    }
    history.retain(|existing| existing != value);
    history.insert(0, value.to_string());
    history.truncate(limit);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn replacement(pattern: &str, with: &str) -> Replacement {
        Replacement {
            pattern: pattern.to_string(),
            replacement: with.to_string(),
        }
    }

    #[test]
    fn every_match_is_replaced_and_counted() {
        let result = apply("a-b-c", &replacement("-", "+")).expect("valid");
        assert_eq!(result.text, "a+b+c");
        assert_eq!(result.matches, 2);
    }

    #[test]
    fn capture_groups_are_available_to_the_replacement() {
        let result = apply("Smith, John", &replacement(r"(\w+), (\w+)", "$2 $1")).expect("valid");
        assert_eq!(result.text, "John Smith");
    }

    #[test]
    fn a_named_group_works_too() {
        let result = apply("2026-09-25", &replacement(r"(?<y>\d{4})-\d\d-\d\d", "$y")).expect("v");
        assert_eq!(result.text, "2026");
    }

    #[test]
    fn text_with_no_match_comes_back_unchanged_and_says_so() {
        let result = apply("nothing here", &replacement("zzz", "x")).expect("valid");
        assert_eq!(result.text, "nothing here");
        assert_eq!(result.matches, 0);
    }

    #[test]
    fn an_impossible_pattern_is_an_error_rather_than_a_panic() {
        assert!(apply("x", &replacement("([", "y")).is_err());
        assert!(!is_valid("(["));
        assert!(is_valid(r"\d+"));
        assert!(!is_valid(""), "an empty pattern matches everywhere");
    }

    #[test]
    fn a_broken_pattern_is_explained_in_one_line() {
        let reason = why_invalid("([").expect("it does not compile");
        assert!(!reason.contains('\n'), "{reason}");
        assert!(!reason.is_empty());
        assert_eq!(why_invalid(r"\d"), None);
    }

    #[test]
    fn history_keeps_the_most_recent_first_without_duplicates() {
        let mut history = Vec::new();
        remember(&mut history, "a", 3);
        remember(&mut history, "b", 3);
        remember(&mut history, "a", 3);
        assert_eq!(history, ["a", "b"]);
    }

    #[test]
    fn history_stops_at_the_limit() {
        let mut history = Vec::new();
        for value in ["a", "b", "c", "d"] {
            remember(&mut history, value, 3);
        }
        assert_eq!(history, ["d", "c", "b"]);
    }

    #[test]
    fn an_empty_value_is_not_worth_remembering() {
        let mut history = vec!["a".to_string()];
        remember(&mut history, "", 10);
        assert_eq!(history, ["a"]);
    }
}

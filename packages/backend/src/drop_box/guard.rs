//! The sensitive-file rules (`guard_rules`): which items are placed by
//! their name only, or not sent to the model at all.
//!
//! A pattern is a glob (globset): `*` any characters (`/` included), `?`
//! one, `[abc]` one of these, `{a,b}` either; case is ignored, and `\`
//! makes the next character plain (`\*` is a star). A keyword is a word
//! that appears in the text, ignoring case.

use crate::routes::drop_box::{GuardRule, RuleAction, RuleKind};

/// What is known of an item when the rules are checked.
#[derive(Default)]
pub struct Subject<'a> {
    pub name: &'a str,
    /// Its path inside a dropped folder, e.g. `Taxes/2025/return.pdf`.
    pub path: Option<&'a str>,
    pub note: Option<&'a str>,
    /// The text read from it, once it was read.
    pub text: Option<&'a str>,
}

/// The strictest action of the rules that match: `by_hand` before
/// `name_only`; `None` when no rule matches.
pub fn check(rules: &[GuardRule], subject: &Subject) -> Option<RuleAction> {
    let mut action = None;
    for rule in rules.iter().filter(|r| matches(r, subject)) {
        match rule.action {
            RuleAction::ByHand => return Some(RuleAction::ByHand),
            RuleAction::NameOnly => action = Some(RuleAction::NameOnly),
        }
    }
    action
}

fn matches(rule: &GuardRule, subject: &Subject) -> bool {
    match rule.kind {
        RuleKind::Name => wildcard(&rule.pattern, subject.name),
        RuleKind::Path => subject.path.is_some_and(|p| wildcard(&rule.pattern, p)),
        RuleKind::Keyword => [Some(subject.name), subject.note, subject.text]
            .into_iter()
            .flatten()
            .any(|text| has_word(text, &rule.pattern)),
    }
}

/// A pattern compiled as the rules read it; an error says what is wrong
/// with it.
pub fn compile(pattern: &str) -> Result<globset::GlobMatcher, String> {
    // globset ignores case for ASCII only: both sides are lower-cased
    // instead, for every letter.
    globset::GlobBuilder::new(&pattern.to_lowercase())
        .case_insensitive(true)
        .literal_separator(false)
        .backslash_escape(true)
        .build()
        .map(|glob| glob.compile_matcher())
        .map_err(|e| e.kind().to_string())
}

/// Whether `text` matches `pattern`; a pattern that does not compile
/// matches nothing (rules are checked when they are added).
pub fn wildcard(pattern: &str, text: &str) -> bool {
    compile(pattern).is_ok_and(|glob| glob.is_match(text.to_lowercase()))
}

/// Whether `word` (one or more words) appears in `text` on word
/// boundaries, ignoring case: `tax` is in "Tax return", not in "taxi".
pub fn has_word(text: &str, word: &str) -> bool {
    let text = text.to_lowercase();
    let word = word.trim().to_lowercase();
    if word.is_empty() {
        return false;
    }
    let boundary = |c: Option<char>| c.is_none_or(|c| !c.is_alphanumeric());
    text.match_indices(&word).any(|(at, _)| {
        boundary(text[..at].chars().next_back()) && boundary(text[at + word.len()..].chars().next())
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use uuid::Uuid;

    #[test]
    fn wildcards_match_any_characters_ignoring_case() {
        assert!(wildcard("*.kdbx", "Passwords.KDBX"));
        assert!(wildcard("passport*", "Passport scan 2024.pdf"));
        assert!(wildcard("*/tax/*", "Papers/Tax/2025/return.pdf"));
        assert!(wildcard("invoice-????.pdf", "invoice-2025.pdf"));
        assert!(wildcard("*", ""));
        assert!(wildcard("a*b*c", "aXXbYYc"));
        assert!(!wildcard("a*b*c", "aXXbYY"));
        assert!(!wildcard("*.kdbx", "kdbx.txt"));
        assert!(!wildcard("invoice-????.pdf", "invoice-25.pdf"));
        assert!(wildcard("ærlig*", "ÆRLIG svar.txt"));
        assert!(wildcard("report-[0-9].pdf", "report-7.pdf"));
        assert!(wildcard("*.{jpg,png}", "photo.PNG"));
        assert!(wildcard("a\\*b", "a*b") && !wildcard("a\\*b", "axb"));
        assert!(compile("[unclosed").is_err());
    }

    #[test]
    fn keywords_are_whole_words() {
        assert!(has_word("My tax return", "tax"));
        assert!(has_word("TAX", "tax"));
        assert!(has_word("Social security number: 123", "social security"));
        assert!(!has_word("taxi receipt", "tax"));
        assert!(!has_word("syntax", "tax"));
        assert!(!has_word("anything", "  "));
    }

    fn rule(kind: RuleKind, pattern: &str, action: RuleAction) -> GuardRule {
        GuardRule {
            id: Uuid::now_v7(),
            kind,
            pattern: pattern.into(),
            action,
            created_at: String::new(),
        }
    }

    #[test]
    fn the_strictest_rule_that_matches_wins() {
        let rules = [
            rule(RuleKind::Keyword, "passport", RuleAction::NameOnly),
            rule(RuleKind::Name, "*.kdbx", RuleAction::ByHand),
            rule(RuleKind::Path, "*/private/*", RuleAction::ByHand),
        ];
        let subject = |name, note, text| Subject {
            name,
            note,
            text,
            path: None,
        };
        assert_eq!(check(&rules, &subject("scan.pdf", None, None)), None);
        assert_eq!(
            check(&rules, &subject("scan.pdf", Some("my passport"), None)),
            Some(RuleAction::NameOnly)
        );
        assert_eq!(
            check(&rules, &subject("scan.pdf", None, Some("PASSPORT No. 1"))),
            Some(RuleAction::NameOnly)
        );
        assert_eq!(
            check(&rules, &subject("passport.kdbx", None, None)),
            Some(RuleAction::ByHand)
        );
        let inside = Subject {
            name: "a.txt",
            path: Some("Box/private/a.txt"),
            ..Subject::default()
        };
        assert_eq!(check(&rules, &inside), Some(RuleAction::ByHand));
    }
}

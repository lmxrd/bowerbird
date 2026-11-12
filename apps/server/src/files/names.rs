//! File and folder names (docs/ARCHITECTURE.md §4.6): stored in Unicode NFC,
//! portable (Windows-safe) when Organizer makes them, and never two in one
//! folder that differ only in case when Organizer makes them. Names made
//! outside Organizer are kept as they are and flagged when not portable.

use unicode_normalization::{UnicodeNormalization, is_nfc};

/// The most bytes a name may have (most filesystems allow 255).
const MAX_BYTES: usize = 255;

/// Names Windows refuses for a file of any extension.
const RESERVED: [&str; 22] = [
    "CON", "PRN", "AUX", "NUL", "COM1", "COM2", "COM3", "COM4", "COM5", "COM6", "COM7", "COM8",
    "COM9", "LPT1", "LPT2", "LPT3", "LPT4", "LPT5", "LPT6", "LPT7", "LPT8", "LPT9",
];

/// The name in NFC: what a macOS client's decomposed "é" (e + accent)
/// becomes, so that it is the same name as everyone else's.
pub fn nfc(name: &str) -> String {
    if is_nfc(name) {
        name.to_string()
    } else {
        name.nfc().collect()
    }
}

/// The key two names collide on when they differ only in case: NFC, then
/// Unicode lowercase.
pub fn fold(name: &str) -> String {
    nfc(name).to_lowercase()
}

/// Whether a name works on every platform the clients run on: no
/// character Windows refuses, no reserved device name, no trailing dot or
/// space, at most 255 bytes.
pub fn is_portable(name: &str) -> bool {
    if name.is_empty() || name.len() > MAX_BYTES || name == "." || name == ".." {
        return false;
    }
    if name.chars().any(|c| {
        c.is_control() || matches!(c, '<' | '>' | ':' | '"' | '/' | '\\' | '|' | '?' | '*')
    }) {
        return false;
    }
    if name.ends_with('.') || name.ends_with(' ') {
        return false;
    }
    let stem = name.split('.').next().unwrap_or("").trim_end();
    !RESERVED.iter().any(|r| r.eq_ignore_ascii_case(stem))
}

/// A name for something Organizer makes (a folder, an upload, a rename):
/// trimmed of surrounding spaces, in NFC, and portable. The error is fit to
/// show.
pub fn new_name(raw: &str) -> Result<String, String> {
    let name = nfc(raw.trim());
    if name.is_empty() {
        return Err("A name is required".into());
    }
    if name.len() > MAX_BYTES {
        return Err("The name is too long".into());
    }
    if !is_portable(&name) {
        return Err(
            "The name has a character or form other systems refuse (< > : \" / \\ | ? *, a \
             reserved name such as CON, or a trailing dot or space)"
                .into(),
        );
    }
    Ok(name)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_are_stored_in_nfc() {
        let decomposed = "Cafe\u{301}.txt";
        assert_eq!(nfc(decomposed), "Café.txt");
        assert_eq!(nfc("Café.txt"), "Café.txt");
        assert_eq!(fold("CAFE\u{301}.TXT"), fold("café.txt"));
    }

    #[test]
    fn portable_names_work_on_every_platform() {
        for good in [
            "report.pdf",
            "Ærlig talt",
            ".hidden",
            "a b",
            "con-tract.pdf",
            "CONSOLE",
        ] {
            assert!(is_portable(good), "{good}");
        }
        for bad in [
            "",
            ".",
            "..",
            "a:b",
            "what?",
            "a/b",
            "a\\b",
            "x*",
            "trailing.",
            "trailing ",
            "CON",
            "nul.txt",
            "Com1.log",
            "tab\there",
        ] {
            assert!(!is_portable(bad), "{bad:?}");
        }
        assert!(!is_portable(&"a".repeat(256)));
    }

    #[test]
    fn new_names_are_trimmed_normalized_and_checked() {
        assert_eq!(new_name("  Cafe\u{301}  ").unwrap(), "Café");
        assert!(new_name("   ").is_err());
        assert!(new_name("a:b").is_err());
    }
}

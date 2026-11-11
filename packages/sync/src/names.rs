//! Local names for the server's (ARCHITECTURE.md §4.6): a name this system
//! refuses, or a case-only twin on a case-insensitive disk, is given a safe
//! local name. The engine keeps the local name of each synced node in its
//! state, so a mapped name stays as it is until the server's name changes,
//! and is never sent back as a rename.

use std::collections::HashSet;
use unicode_normalization::UnicodeNormalization;

/// What the local disk refuses or confuses.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Naming {
    /// Windows' rules: no `< > : " / \ | ? *` or control characters, no
    /// trailing dot or space, no reserved device names (`CON`, `NUL`, ...).
    pub windows: bool,
    /// Names that differ only in case are the same file (macOS and Windows
    /// by default).
    pub case_insensitive: bool,
}

impl Naming {
    /// The rules of this system, for a folder on a disk that is
    /// case-insensitive or not (see [`case_insensitive`]).
    pub fn for_disk(case_insensitive: bool) -> Self {
        Naming {
            windows: cfg!(windows),
            case_insensitive,
        }
    }

    fn key(&self, name: &str) -> String {
        let name = nfc(name);
        if self.case_insensitive {
            name.to_lowercase()
        } else {
            name
        }
    }
}

/// A name in NFC, as the server stores names.
pub fn nfc(name: &str) -> String {
    name.nfc().collect()
}

const RESERVED: &[&str] = &[
    "CON", "PRN", "AUX", "NUL", "COM1", "COM2", "COM3", "COM4", "COM5", "COM6", "COM7", "COM8",
    "COM9", "LPT1", "LPT2", "LPT3", "LPT4", "LPT5", "LPT6", "LPT7", "LPT8", "LPT9",
];

/// A name this system accepts: each refused character becomes `_`, and a
/// reserved name gets a `_` after its stem.
pub fn portable(name: &str, naming: Naming) -> String {
    if !naming.windows {
        return name.replace('/', "_");
    }
    let mut out: String = name
        .chars()
        .map(|c| {
            if c.is_control() || "<>:\"/\\|?*".contains(c) {
                '_'
            } else {
                c
            }
        })
        .collect();
    let kept = out.trim_end_matches(['.', ' ']).len();
    if kept < out.len() {
        out = format!("{}{}", &out[..kept], "_".repeat(out.len() - kept));
    }
    let stem = out.split('.').next().unwrap_or("").to_ascii_uppercase();
    if RESERVED.contains(&stem.as_str()) {
        let (stem, rest) = out.split_at(stem.len());
        out = format!("{stem}_{rest}");
    }
    out
}

/// The names taken in one local folder, as the disk compares them.
#[derive(Default)]
pub struct Taken {
    keys: HashSet<String>,
}

impl Taken {
    pub fn insert(&mut self, name: &str, naming: Naming) {
        self.keys.insert(naming.key(name));
    }

    pub fn contains(&self, name: &str, naming: Naming) -> bool {
        self.keys.contains(&naming.key(name))
    }

    /// The local name for a server name in this folder: portable, and
    /// with ` (2)`, ` (3)`, ... before the extension when it is taken (a
    /// case-only twin on a case-insensitive disk). The name is then taken.
    pub fn local_name(&mut self, remote: &str, naming: Naming) -> String {
        let base = portable(remote, naming);
        let mut name = base.clone();
        let mut n = 2;
        while self.contains(&name, naming) {
            name = with_suffix(&base, &format!(" ({n})"));
            n += 1;
        }
        self.insert(&name, naming);
        name
    }
}

/// `suffix` before the extension: `Report.docx` → `Report (2).docx`. A
/// leading dot is not an extension (`.profile`).
pub fn with_suffix(name: &str, suffix: &str) -> String {
    match name.rfind('.') {
        Some(i) if i > 0 => format!("{}{suffix}{}", &name[..i], &name[i..]),
        _ => format!("{name}{suffix}"),
    }
}

/// The name of a conflict copy: `Report (conflict, MacBook, 2026-11-07).docx`.
pub fn conflict_name(name: &str, device: &str, date: &str) -> String {
    with_suffix(name, &format!(" (conflict, {device}, {date})"))
}

/// Whether the disk of this folder is case-insensitive: a file made in
/// lower case is looked for in upper case.
pub fn case_insensitive(dir: &std::path::Path) -> bool {
    let probe = dir.join(format!(".organizer-case-{}", uuid::Uuid::new_v4().simple()));
    if std::fs::write(&probe, b"").is_err() {
        return cfg!(any(windows, target_os = "macos"));
    }
    let upper = dir.join(
        probe
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or_default()
            .to_uppercase(),
    );
    let insensitive = upper.exists();
    let _ = std::fs::remove_file(&probe);
    insensitive
}

#[cfg(test)]
mod tests {
    use super::*;

    const WINDOWS: Naming = Naming {
        windows: true,
        case_insensitive: true,
    };
    const LINUX: Naming = Naming {
        windows: false,
        case_insensitive: false,
    };

    #[test]
    fn windows_names_are_made_portable() {
        assert_eq!(portable("a:b?.txt", WINDOWS), "a_b_.txt");
        assert_eq!(portable("notes.", WINDOWS), "notes_");
        assert_eq!(portable("CON.txt", WINDOWS), "CON_.txt");
        assert_eq!(portable("con", WINDOWS), "con_");
        assert_eq!(portable("Console.txt", WINDOWS), "Console.txt");
        assert_eq!(portable("a:b", LINUX), "a:b");
    }

    #[test]
    fn case_only_twins_get_a_number_where_case_does_not_count() {
        let mut taken = Taken::default();
        assert_eq!(taken.local_name("Report.pdf", WINDOWS), "Report.pdf");
        assert_eq!(taken.local_name("report.pdf", WINDOWS), "report (2).pdf");
        let mut taken = Taken::default();
        assert_eq!(taken.local_name("Report.pdf", LINUX), "Report.pdf");
        assert_eq!(taken.local_name("report.pdf", LINUX), "report.pdf");
    }

    #[test]
    fn conflict_copies_keep_the_extension() {
        assert_eq!(
            conflict_name("Report.docx", "MacBook", "2026-11-07"),
            "Report (conflict, MacBook, 2026-11-07).docx"
        );
        assert_eq!(
            conflict_name("Makefile", "Pc", "2026-11-07"),
            "Makefile (conflict, Pc, 2026-11-07)"
        );
        assert_eq!(
            conflict_name(".env", "Pc", "2026-11-07"),
            ".env (conflict, Pc, 2026-11-07)"
        );
    }
}

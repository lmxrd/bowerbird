//! The files that are never synced (M5_DESIGN.md §2): the systems' own
//! files, office lock files and partial downloads. The user sees and edits
//! the list in the settings.
//!
//! The patterns are matched against a file's name only, with `*` for any
//! run of characters: that is all the list needs, so a glob crate would
//! be more than the job.

/// The list a new account starts with.
pub const DEFAULT: &[&str] = &[
    ".DS_Store",
    "Thumbs.db",
    "desktop.ini",
    "~$*",
    "*.tmp",
    ".~lock.*#",
    "*.crdownload",
    "*.part",
    "*.partial",
    "*.download",
];

/// The engine's own temporary files: always ignored, whatever the list.
pub const OWN_PREFIX: &str = ".organizer-";

/// The server's own folder at a drive's root (its trash and versions,
/// docs/M7A_DESIGN.md §1): never listed by the server, and a local one at
/// a synced drive's root is never sent (the server refuses the name there).
pub const DRIVE_DIR: &str = ".organizer";

#[derive(Clone, Debug, Default)]
pub struct Ignore {
    patterns: Vec<String>,
    /// The synced folder is a drive's root: its `.organizer` is left alone.
    drive_root: bool,
}

impl Ignore {
    pub fn new(patterns: impl IntoIterator<Item = impl Into<String>>) -> Self {
        Ignore {
            patterns: patterns
                .into_iter()
                .map(Into::into)
                .filter(|p: &String| !p.trim().is_empty())
                .collect(),
            drive_root: false,
        }
    }

    /// The same list, for a synced folder that is a drive's root or not.
    pub fn at_drive_root(mut self, drive_root: bool) -> Self {
        self.drive_root = drive_root;
        self
    }

    pub fn defaults() -> Self {
        Self::new(DEFAULT.iter().copied())
    }

    pub fn patterns(&self) -> &[String] {
        &self.patterns
    }

    /// Whether a file or folder of this name is left alone.
    pub fn ignored(&self, name: &str) -> bool {
        name.starts_with(OWN_PREFIX) || self.patterns.iter().any(|p| matches(p.trim(), name))
    }

    /// Whether `name`, in the folder at `parent` (relative to the synced
    /// folder; empty at its top), is left alone.
    pub fn ignored_in(&self, parent: &str, name: &str) -> bool {
        // In any case, as the server compares names.
        self.ignored(name)
            || (self.drive_root && parent.is_empty() && name.eq_ignore_ascii_case(DRIVE_DIR))
    }

    /// Whether a path relative to the synced folder (`a/b/c`), or a folder
    /// it is in, is left alone.
    pub fn ignores_path(&self, path: &str) -> bool {
        let top = path.split('/').next().unwrap_or_default();
        self.ignored_in("", top) || path.split('/').any(|name| self.ignored(name))
    }
}

/// `*` matches any run of characters (none included); everything else
/// itself.
fn matches(pattern: &str, name: &str) -> bool {
    let parts: Vec<&str> = pattern.split('*').collect();
    if parts.len() == 1 {
        return pattern == name;
    }
    let (first, last) = (parts[0], parts[parts.len() - 1]);
    if !name.starts_with(first) || name.len() < first.len() + last.len() || !name.ends_with(last) {
        return false;
    }
    let mut rest = &name[first.len()..name.len() - last.len()];
    for part in &parts[1..parts.len() - 1] {
        match rest.find(part) {
            Some(i) => rest = &rest[i + part.len()..],
            None => return false,
        }
    }
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_default_list_leaves_the_systems_files_alone() {
        let ignore = Ignore::defaults();
        for name in [
            ".DS_Store",
            "Thumbs.db",
            "~$Report.docx",
            "draft.tmp",
            ".~lock.Report.odt#",
            "movie.mp4.crdownload",
            ".organizer-download-1",
        ] {
            assert!(ignore.ignored(name), "{name}");
        }
        for name in ["Report.docx", "tmp", "lock.odt", "a.tmpl"] {
            assert!(!ignore.ignored(name), "{name}");
        }
    }

    #[test]
    fn the_servers_own_folder_is_left_alone_at_a_drives_root_only() {
        let plain = Ignore::defaults();
        assert!(!plain.ignored_in("", ".organizer"));
        assert!(!plain.ignores_path(".organizer/a.txt"));
        let drive = Ignore::defaults().at_drive_root(true);
        assert!(drive.ignored_in("", ".organizer"));
        assert!(drive.ignores_path(".organizer"));
        assert!(drive.ignores_path(".organizer/trash/a.txt"));
        assert!(drive.ignored_in("", ".Organizer"));
        assert!(!drive.ignored_in("Docs", ".organizer"));
        assert!(!drive.ignores_path("Docs/.organizer"));
        assert!(!drive.ignores_path(".organizers"));
        assert!(drive.ignores_path("Docs/.DS_Store"));
    }

    #[test]
    fn stars_match_any_run() {
        assert!(matches("a*b*c", "abc"));
        assert!(matches("a*b*c", "a-b-c"));
        assert!(!matches("a*b*c", "a-c"));
        assert!(matches("*", "anything"));
        assert!(!matches("ab*ba", "aba"));
    }
}

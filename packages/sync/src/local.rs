//! The local side: a folder's scan, a file's hash, and deletes to the
//! trash.

use crate::ignore::Ignore;
use std::collections::BTreeMap;
use std::io;
use std::path::{Path, PathBuf};
use std::str::FromStr;

#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Kind {
    File,
    Folder,
}

impl Kind {
    pub fn as_str(self) -> &'static str {
        match self {
            Kind::File => "file",
            Kind::Folder => "folder",
        }
    }
}

impl FromStr for Kind {
    type Err = ();
    fn from_str(s: &str) -> Result<Self, ()> {
        match s {
            "file" => Ok(Kind::File),
            "folder" => Ok(Kind::Folder),
            _ => Err(()),
        }
    }
}

/// A file or folder on the local disk, as a scan sees it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Stat {
    pub kind: Kind,
    pub size: i64,
    pub mtime_ns: i64,
    /// The inode (Unix), which a move keeps; none on Windows, where the
    /// file id needs an API std does not have stable yet: a moved file is
    /// recognised by its hash there, and a moved folder is sent again.
    pub inode: Option<i64>,
}

impl Stat {
    pub fn of(metadata: &std::fs::Metadata) -> Self {
        Stat {
            kind: if metadata.is_dir() {
                Kind::Folder
            } else {
                Kind::File
            },
            size: if metadata.is_dir() {
                0
            } else {
                metadata.len() as i64
            },
            mtime_ns: mtime_ns(metadata),
            inode: inode(metadata),
        }
    }

    /// Whether the content may have changed since this one: another size
    /// or modification time.
    pub fn differs(&self, size: Option<i64>, mtime_ns: Option<i64>) -> bool {
        Some(self.size) != size || Some(self.mtime_ns) != mtime_ns
    }
}

pub fn mtime_ns(metadata: &std::fs::Metadata) -> i64 {
    metadata
        .modified()
        .ok()
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|d| d.as_nanos() as i64)
        .unwrap_or(0)
}

#[cfg(unix)]
fn inode(metadata: &std::fs::Metadata) -> Option<i64> {
    use std::os::unix::fs::MetadataExt;
    Some(metadata.ino() as i64)
}

#[cfg(not(unix))]
fn inode(_: &std::fs::Metadata) -> Option<i64> {
    None
}

/// What is at a path now, without following a symbolic link.
pub fn stat(path: &Path) -> Option<Stat> {
    let metadata = std::fs::symlink_metadata(path).ok()?;
    if metadata.file_type().is_symlink() {
        return None;
    }
    Some(Stat::of(&metadata))
}

/// Everything under a folder, by path relative to it (`/` between the
/// names): the ignored names and symbolic links left out, and names that
/// are not Unicode (which the server could not hold).
pub fn scan(root: &Path, ignore: &Ignore) -> io::Result<BTreeMap<String, Stat>> {
    let mut out = BTreeMap::new();
    let mut stack = vec![(root.to_path_buf(), String::new())];
    while let Some((dir, prefix)) = stack.pop() {
        let entries = match std::fs::read_dir(&dir) {
            Ok(entries) => entries,
            // A folder that went away during the scan: the next one sees it.
            Err(e) if e.kind() == io::ErrorKind::NotFound && dir != root => continue,
            Err(e) => return Err(e),
        };
        for entry in entries {
            let entry = entry?;
            let Some(name) = entry.file_name().to_str().map(String::from) else {
                tracing::warn!("skipped a name that is not Unicode in {}", dir.display());
                continue;
            };
            if ignore.ignored_in(&prefix, &name) {
                continue;
            }
            let Ok(metadata) = std::fs::symlink_metadata(entry.path()) else {
                continue;
            };
            if metadata.file_type().is_symlink() {
                continue;
            }
            let path = if prefix.is_empty() {
                name
            } else {
                format!("{prefix}/{name}")
            };
            let stat = Stat::of(&metadata);
            if stat.kind == Kind::Folder {
                stack.push((entry.path(), path.clone()));
            }
            out.insert(path, stat);
        }
    }
    Ok(out)
}

/// A file's BLAKE3 hash, in hex.
pub async fn hash_file(path: &Path) -> io::Result<String> {
    let path = path.to_path_buf();
    tokio::task::spawn_blocking(move || {
        let mut hasher = blake3::Hasher::new();
        let mut file = std::fs::File::open(&path)?;
        io::copy(&mut file, &mut hasher)?;
        Ok(hasher.finalize().to_hex().to_string())
    })
    .await
    .map_err(io::Error::other)?
}

/// Where local deletes go.
#[derive(Clone, Debug)]
pub enum Trash {
    /// The system's trash (Finder's, the Recycle Bin, the freedesktop one).
    System,
    /// A folder of its own, for the tests: the developer's trash stays as
    /// it was.
    Folder(PathBuf),
}

impl Trash {
    /// Move a file or folder to the trash, never deleting it for good.
    pub fn put(&self, path: &Path) -> io::Result<()> {
        match self {
            Trash::System => system_trash(path),
            Trash::Folder(dir) => {
                std::fs::create_dir_all(dir)?;
                let name = path
                    .file_name()
                    .map(|n| n.to_string_lossy().into_owned())
                    .unwrap_or_default();
                let target = dir.join(format!("{}-{name}", uuid::Uuid::new_v4().simple()));
                std::fs::rename(path, target)
            }
        }
    }
}

#[cfg(target_os = "macos")]
fn system_trash(path: &Path) -> io::Result<()> {
    // NSFileManager rather than the default (Finder through AppleScript),
    // which asks for permission to control Finder and plays a sound.
    use trash::macos::{DeleteMethod, TrashContextExtMacos};
    let mut context = trash::TrashContext::default();
    context.set_delete_method(DeleteMethod::NsFileManager);
    context.delete(path).map_err(io::Error::other)
}

#[cfg(not(target_os = "macos"))]
fn system_trash(path: &Path) -> io::Result<()> {
    trash::delete(path).map_err(io::Error::other)
}

/// A folder's path joined with a relative one (`/` between the names).
pub fn join(root: &Path, relative: &str) -> PathBuf {
    let mut path = root.to_path_buf();
    for part in relative.split('/').filter(|p| !p.is_empty()) {
        path.push(part);
    }
    path
}

/// The parent of a relative path; "" for a top-level one.
pub fn parent_of(relative: &str) -> &str {
    relative.rsplit_once('/').map(|(p, _)| p).unwrap_or("")
}

/// The last name of a relative path.
pub fn name_of(relative: &str) -> &str {
    relative
        .rsplit_once('/')
        .map(|(_, n)| n)
        .unwrap_or(relative)
}

/// `parent/name`, or `name` at the top.
pub fn child(parent: &str, name: &str) -> String {
    if parent.is_empty() {
        name.to_string()
    } else {
        format!("{parent}/{name}")
    }
}

/// Whether `path` is `folder` or under it.
pub fn is_within(path: &str, folder: &str) -> bool {
    path == folder || (path.starts_with(folder) && path[folder.len()..].starts_with('/'))
}

/// `path` under `from` moved under `to`.
pub fn rebase(path: &str, from: &str, to: &str) -> String {
    format!("{to}{}", &path[from.len()..])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_scan_leaves_out_the_ignored_names() {
        let dir = std::env::temp_dir().join(format!("organizer-scan-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(dir.join("a/b")).unwrap();
        std::fs::write(dir.join("a/b/c.txt"), b"hi").unwrap();
        std::fs::write(dir.join("a/.DS_Store"), b"").unwrap();
        let found = scan(&dir, &Ignore::defaults()).unwrap();
        let paths: Vec<&str> = found.keys().map(String::as_str).collect();
        assert_eq!(paths, ["a", "a/b", "a/b/c.txt"]);
        assert_eq!(found["a/b/c.txt"].size, 2);
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn relative_paths() {
        assert_eq!(parent_of("a/b/c"), "a/b");
        assert_eq!(parent_of("c"), "");
        assert_eq!(name_of("a/b/c"), "c");
        assert!(is_within("a/b", "a"));
        assert!(!is_within("ab", "a"));
        assert_eq!(rebase("a/b/c", "a/b", "x"), "x/c");
    }
}

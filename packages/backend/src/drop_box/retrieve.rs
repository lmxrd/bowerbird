//! The part of the user's tree the model is shown for an item
//! (docs/ARCHITECTURE.md §5, "retrieve context"): the top of the tree, and
//! the folders most like the item, with a few of the file names in each.
//!
//! Without embeddings the likeness is lexical: the words of the item (its
//! name, its note, the start of its text) against the words of each
//! folder's path and of the names of the files in it, rare words counting
//! more than common ones. With embeddings (later), the nearest folders by
//! vector join them.

use std::collections::{HashMap, HashSet};
use uuid::Uuid;

/// How many folders like the item are shown, besides the top of the tree.
pub const CANDIDATES: usize = 30;
/// A tree of at most this many folders is shown whole.
pub const WHOLE_TREE: usize = 150;
/// How many file names are shown as examples of a folder.
pub const EXAMPLES: usize = 5;
/// How much of the item's text its words are taken from, in characters.
const TEXT_WORDS_FROM: usize = 4_000;

/// A folder of the user's tree.
#[derive(Clone, Debug)]
pub struct Folder {
    pub id: Uuid,
    /// From the root, without it: `["Bills", "Power"]`.
    pub path: Vec<String>,
    /// Names of files in it, the newest first, at most a few.
    pub examples: Vec<String>,
}

impl Folder {
    pub fn display(&self) -> String {
        self.path.join("/")
    }
}

/// What the model is shown of the tree.
#[derive(Debug, Default)]
pub struct Context {
    /// The folders shown, in tree order, each once.
    pub folders: Vec<Folder>,
    /// The ids of the folders most like the item, the most like first.
    pub ranked: Vec<Uuid>,
}

impl Context {
    /// The folders as an outline for the prompt: one per line, its path,
    /// and a few of its files.
    pub fn outline(&self) -> String {
        let mut out = String::new();
        for folder in &self.folders {
            out.push_str(&folder.display());
            if !folder.examples.is_empty() {
                out.push_str("  (e.g. ");
                out.push_str(&folder.examples.join(", "));
                out.push(')');
            }
            out.push('\n');
        }
        out
    }

    /// The folder at a path shown (case and Unicode form as typed by the
    /// model may differ from the disk's: compared folded).
    pub fn find(&self, path: &str) -> Option<&Folder> {
        let wanted = fold_path(path);
        self.folders
            .iter()
            .find(|f| fold_path(&f.display()) == wanted)
    }
}

fn fold_path(path: &str) -> String {
    path.trim_matches('/')
        .split('/')
        .map(|part| crate::files::names::fold(part.trim()))
        .collect::<Vec<_>>()
        .join("/")
}

/// The words of a text, for matching: lower case, split at anything that
/// is not a letter or a digit and between letters and digits, without the
/// short and common ones; a plural `s` is dropped.
pub fn words(text: &str) -> Vec<String> {
    let mut out = Vec::new();
    for raw in text.split(|c: char| !c.is_alphanumeric()) {
        // "invoice2025" is two words.
        let mut part = String::new();
        let mut digits = None;
        for c in raw.chars() {
            let is_digit = c.is_ascii_digit();
            if digits.is_some_and(|d| d != is_digit) && !part.is_empty() {
                push_word(&mut out, &part);
                part.clear();
            }
            digits = Some(is_digit);
            part.extend(c.to_lowercase());
        }
        push_word(&mut out, &part);
    }
    out
}

fn push_word(out: &mut Vec<String>, word: &str) {
    let is_number = word.chars().all(|c| c.is_ascii_digit());
    // Numbers count as years only; other words from three letters on.
    let keep = if is_number {
        word.len() == 4
    } else {
        word.chars().count() >= 3 && !STOP_WORDS.contains(&word)
    };
    if keep {
        let word = match word.strip_suffix('s') {
            Some(stem) if !is_number && stem.chars().count() >= 4 && !stem.ends_with('s') => stem,
            _ => word,
        };
        out.push(word.to_string());
    }
}

/// Words too common to tell folders apart (English; the rarity weighting
/// takes care of the rest, in any language).
const STOP_WORDS: &[&str] = &[
    "the",
    "and",
    "for",
    "you",
    "your",
    "with",
    "from",
    "this",
    "that",
    "are",
    "was",
    "were",
    "have",
    "has",
    "not",
    "but",
    "all",
    "any",
    "can",
    "our",
    "out",
    "new",
    "copy",
    "file",
    "files",
    "folder",
    "document",
    "documents",
    "page",
    "pdf",
    "docx",
    "xlsx",
    "txt",
    "jpg",
    "jpeg",
    "png",
    "scan",
    "final",
    "untitled",
];

/// The words of an item: its name, its note and the start of its text.
pub fn item_words(name: &str, note: Option<&str>, text: Option<&str>) -> Vec<String> {
    let mut all = words(name);
    if let Some(note) = note {
        all.extend(words(note));
    }
    if let Some(text) = text {
        all.extend(words(&super::extract::truncate(text, TEXT_WORDS_FROM)));
    }
    all
}

/// The folders to show for an item's words, from all the user's folders
/// (only those under the allowed top-level folders, when the user limits
/// them).
pub fn context(all: Vec<Folder>, item: &[String], allowed_top: Option<&[String]>) -> Context {
    context_with(all, item, allowed_top, &[])
}

/// [`context`], with the folders nearest by embeddings too: they and the
/// lexical matches take turns among the best.
pub fn context_with(
    all: Vec<Folder>,
    item: &[String],
    allowed_top: Option<&[String]>,
    semantic: &[Uuid],
) -> Context {
    let folders: Vec<Folder> = all
        .into_iter()
        .filter(|f| !f.path.is_empty())
        .filter(|f| !f.path.iter().any(|p| p.starts_with('.')))
        .filter(|f| {
            allowed_top.is_none_or(|top| {
                top.iter()
                    .any(|t| crate::files::names::fold(t) == crate::files::names::fold(&f.path[0]))
            })
        })
        .collect();
    let lexical = rank(&folders, item);
    let known: HashSet<Uuid> = folders.iter().map(|f| f.id).collect();
    let semantic: Vec<Uuid> = semantic
        .iter()
        .copied()
        .filter(|id| known.contains(id))
        .collect();
    let mut ranked = Vec::new();
    let mut seen = HashSet::new();
    for i in 0..lexical.len().max(semantic.len()) {
        for id in [semantic.get(i), lexical.get(i)].into_iter().flatten() {
            if seen.insert(*id) {
                ranked.push(*id);
            }
        }
    }

    let mut shown: HashSet<Uuid> = HashSet::new();
    if folders.len() <= WHOLE_TREE {
        shown.extend(folders.iter().map(|f| f.id));
    } else {
        // The top level, then the best matches with their parents.
        shown.extend(folders.iter().filter(|f| f.path.len() == 1).map(|f| f.id));
        let by_path: HashMap<Vec<String>, Uuid> =
            folders.iter().map(|f| (f.path.clone(), f.id)).collect();
        for id in ranked.iter().take(CANDIDATES) {
            let folder = folders
                .iter()
                .find(|f| f.id == *id)
                .expect("ranked from them");
            for depth in 1..=folder.path.len() {
                if let Some(parent) = by_path.get(&folder.path[..depth]) {
                    shown.insert(*parent);
                }
            }
        }
    }
    let mut folders: Vec<Folder> = folders
        .into_iter()
        .filter(|f| shown.contains(&f.id))
        .collect();
    folders.sort_by(|a, b| a.path.cmp(&b.path));
    Context {
        folders,
        ranked: ranked.into_iter().take(CANDIDATES).collect(),
    }
}

/// The folders that share words with the item, the most alike first. A
/// word counts by its rarity among the folders (inverse document
/// frequency); a word of the folder's own name counts double, of a parent
/// once, of a file in it half.
pub fn rank(folders: &[Folder], item: &[String]) -> Vec<Uuid> {
    let item: HashSet<&str> = item.iter().map(String::as_str).collect();
    if item.is_empty() {
        return Vec::new();
    }
    // Each folder's words, by where they come from.
    let described: Vec<(Uuid, HashMap<String, f64>)> = folders
        .iter()
        .map(|folder| {
            let mut weights: HashMap<String, f64> = HashMap::new();
            let mut add = |text: &str, weight: f64| {
                for word in words(text) {
                    let w = weights.entry(word).or_default();
                    *w = w.max(weight);
                }
            };
            for example in &folder.examples {
                add(example, 0.5);
            }
            for (i, part) in folder.path.iter().enumerate() {
                add(part, if i + 1 == folder.path.len() { 2.0 } else { 1.0 });
            }
            (folder.id, weights)
        })
        .collect();
    let mut frequency: HashMap<&str, usize> = HashMap::new();
    for (_, weights) in &described {
        for word in weights.keys() {
            *frequency.entry(word.as_str()).or_default() += 1;
        }
    }
    let n = folders.len() as f64;
    let mut scored: Vec<(Uuid, f64, usize)> = described
        .iter()
        .map(|(id, weights)| {
            let score: f64 = weights
                .iter()
                .filter(|(word, _)| item.contains(word.as_str()))
                .map(|(word, weight)| {
                    let df = frequency[word.as_str()] as f64;
                    weight * (1.0 + (n / df).ln())
                })
                .sum();
            let depth = folders
                .iter()
                .find(|f| f.id == *id)
                .map_or(0, |f| f.path.len());
            (*id, score, depth)
        })
        .filter(|(_, score, _)| *score > 0.0)
        .collect();
    // The higher score first; between equals, the deeper (more specific).
    scored.sort_by(|a, b| b.1.total_cmp(&a.1).then(b.2.cmp(&a.2)));
    scored.into_iter().map(|(id, _, _)| id).collect()
}

/// Every folder of a user's My drive, with its path and a few of its files.
pub async fn folders_of(db: &crate::db::Db, user_id: Uuid) -> Result<Vec<Folder>, sqlx::Error> {
    #[derive(sqlx::FromRow)]
    struct Row {
        id: Uuid,
        parent_id: Option<Uuid>,
        name: String,
    }
    let rows: Vec<Row> = sqlx::query_as(
        "SELECT id, parent_id, name FROM nodes
         WHERE drive_id = ?1 AND kind = 'folder' AND e2e_root IS NULL",
    )
    .bind(user_id)
    .fetch_all(db.reader())
    .await?;
    // A few files per folder, the most recently changed first.
    let files: Vec<(Uuid, String)> = sqlx::query_as(
        "SELECT parent_id, name FROM (
           SELECT parent_id, name, row_number() OVER (
             PARTITION BY parent_id ORDER BY mtime_ns DESC, name) AS n
           FROM nodes WHERE drive_id = ?1 AND kind = 'file' AND e2e_root IS NULL)
         WHERE n <= ?2",
    )
    .bind(user_id)
    .bind(EXAMPLES as i64)
    .fetch_all(db.reader())
    .await?;
    let mut examples: HashMap<Uuid, Vec<String>> = HashMap::new();
    for (parent, name) in files {
        examples.entry(parent).or_default().push(name);
    }
    let by_id: HashMap<Uuid, (Option<Uuid>, &str)> = rows
        .iter()
        .map(|r| (r.id, (r.parent_id, r.name.as_str())))
        .collect();
    let path = |id: Uuid| {
        let mut path = Vec::new();
        let mut at = id;
        // The root has no parent and is not part of a path; a cycle cannot
        // happen (a parent is a row of its own), but is cut anyway.
        while let Some((Some(parent), name)) = by_id.get(&at) {
            path.push(name.to_string());
            if path.len() > 256 {
                break;
            }
            at = *parent;
        }
        path.reverse();
        path
    };
    let folders = rows
        .iter()
        .map(|row| Folder {
            id: row.id,
            path: path(row.id),
            examples: examples.remove(&row.id).unwrap_or_default(),
        })
        .collect();
    Ok(folders)
}

/// The folders of the shares a user placed in their files and may add to
/// (docs/M6B_DESIGN.md §5), with their paths as the user sees them: the
/// share's folder and everything under it. Those only in "Shared with me"
/// are not offered. Then the shared drives they are a member of (members
/// may add to them, docs/M7A_DESIGN.md §1): each one's folders, their paths
/// starting with the drive's name.
pub async fn shared_folders(
    files: &crate::files::Files,
    user_id: Uuid,
) -> Result<Vec<Folder>, crate::files::FileError> {
    #[derive(sqlx::FromRow)]
    struct Row {
        id: Uuid,
        parent_id: Option<Uuid>,
        name: String,
    }
    let mut out = Vec::new();
    let mut tops: Vec<(Uuid, Vec<String>)> = files
        .writable_mounts(user_id)
        .await?
        .into_iter()
        .map(|(mount, path)| (mount.node_id, path))
        .collect();
    let drives: Vec<(Uuid, String)> = sqlx::query_as(
        "SELECT DISTINCT n.id, d.name FROM drives d
         JOIN drive_users m ON m.drive_id = d.id AND m.user_id = ?1
           AND m.role IN ('editor', 'manager') AND d.kind = 'shared'
         JOIN nodes n ON n.drive_id = d.id AND n.parent_id IS NULL
         ORDER BY d.name",
    )
    .bind(user_id)
    .fetch_all(files.db().reader())
    .await?;
    tops.extend(drives.into_iter().map(|(root, name)| (root, vec![name])));
    // The drop box skips what cannot be written to now: a drive whose
    // storage fails or is locked, or one being moved (docs/M7C_DESIGN.md §3).
    let mut writable = Vec::with_capacity(tops.len());
    for (top, path) in tops {
        let drive: Option<Uuid> = sqlx::query_scalar("SELECT drive_id FROM nodes WHERE id = ?1")
            .bind(top)
            .fetch_optional(files.db().reader())
            .await?;
        if let Some(drive) = drive
            && !files.writable(drive).await?
        {
            continue;
        }
        writable.push((top, path));
    }
    let tops = writable;
    for (top_id, top) in tops {
        let rows: Vec<Row> = sqlx::query_as(
            "WITH RECURSIVE down (id, parent_id, name) AS (
               SELECT id, parent_id, name FROM nodes
               WHERE parent_id = ?1 AND kind = 'folder' AND e2e_root IS NULL
               UNION ALL
               SELECT n.id, n.parent_id, n.name FROM nodes n JOIN down ON n.parent_id = down.id
               WHERE n.kind = 'folder' AND n.e2e_root IS NULL
             )
             SELECT id, parent_id, name FROM down",
        )
        .bind(top_id)
        .fetch_all(files.db().reader())
        .await?;
        // A limited folder hidden from the user is no place for the drop
        // box, nor what is under it (unless a grant there lets them in).
        let hides: bool = sqlx::query_scalar(
            "SELECT EXISTS (SELECT 1 FROM nodes WHERE limited = 1
                              AND drive_id = (SELECT drive_id FROM nodes WHERE id = ?1))",
        )
        .bind(top_id)
        .fetch_one(files.db().reader())
        .await?;
        let mut rows = rows;
        if hides {
            let mut conn = files.db().reader().acquire().await?;
            let mut seen = Vec::with_capacity(rows.len());
            for row in rows {
                if crate::files::roles::sees(&mut conn, user_id, row.id).await? {
                    seen.push(row);
                }
            }
            rows = seen;
        }
        let mut paths: Vec<(Uuid, Vec<String>)> = vec![(top_id, top)];
        // Parents come before their children in a recursive walk.
        for row in &rows {
            let Some(parent) = row
                .parent_id
                .and_then(|p| paths.iter().find(|(id, _)| *id == p))
                .map(|(_, path)| path.clone())
            else {
                continue;
            };
            let mut path = parent;
            path.push(row.name.clone());
            paths.push((row.id, path));
        }
        for (id, path) in paths {
            let examples: Vec<String> = sqlx::query_scalar(
                "SELECT name FROM nodes WHERE parent_id = ?1 AND kind = 'file' AND e2e_root IS NULL
                 ORDER BY mtime_ns DESC, name LIMIT ?2",
            )
            .bind(id)
            .bind(EXAMPLES as i64)
            .fetch_all(files.db().reader())
            .await?;
            out.push(Folder { id, path, examples });
        }
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn folder(path: &str, examples: &[&str]) -> Folder {
        Folder {
            id: Uuid::now_v7(),
            path: path.split('/').map(String::from).collect(),
            examples: examples.iter().map(|e| e.to_string()).collect(),
        }
    }

    #[test]
    fn words_are_split_folded_and_filtered() {
        assert_eq!(
            words("Invoice2025 - Electricity_Bills (final).PDF"),
            ["invoice", "2025", "electricity", "bill"]
        );
        assert_eq!(words("Ærlig Sønn 12 345"), ["ærlig", "sønn"]);
        assert_eq!(words("glasses business"), ["glasse", "business"]);
    }

    #[test]
    fn the_folders_most_like_the_item_come_first() {
        let tree = vec![
            folder("Home", &[]),
            folder("Home/Bills", &["water-2024.pdf"]),
            folder("Home/Bills/Electricity", &["power 2025-01.pdf"]),
            folder("Health", &["vaccination card.pdf"]),
            folder("Work/Contracts", &["employment contract.pdf"]),
        ];
        let item = item_words(
            "scan_0042.pdf",
            Some("power bill"),
            Some("Electricity invoice, amount due 120"),
        );
        let ranked = rank(&tree, &item);
        assert_eq!(ranked[0], tree[2].id, "Home/Bills/Electricity");
        assert_eq!(ranked[1], tree[1].id, "Home/Bills");
        assert!(!ranked.contains(&tree[3].id));
        assert!(rank(&tree, &[]).is_empty());
    }

    #[test]
    fn a_large_tree_is_cut_to_its_top_and_the_best_matches() {
        let mut tree = vec![folder("Archive", &[]), folder("Travel", &[])];
        for i in 0..200 {
            tree.push(folder(&format!("Archive/Box {i}"), &[]));
        }
        tree.push(folder("Travel/Japan", &[]));
        tree.push(folder("Travel/Japan/Tickets", &["shinkansen.pdf"]));
        let context = context(tree, &words("Japan rail tickets"), None);
        let shown: Vec<String> = context.folders.iter().map(Folder::display).collect();
        assert_eq!(
            shown,
            ["Archive", "Travel", "Travel/Japan", "Travel/Japan/Tickets"]
        );
        assert_eq!(
            context.find("travel/japan/TICKETS/").unwrap().path[2],
            "Tickets"
        );
        assert!(context.find("Travel/Korea").is_none());
        assert!(
            context
                .outline()
                .contains("Travel/Japan/Tickets  (e.g. shinkansen.pdf)\n")
        );
    }

    #[test]
    fn the_nearest_by_embeddings_join_the_best() {
        let mut tree = vec![folder("Archive", &[]), folder("Health", &[])];
        for i in 0..200 {
            tree.push(folder(&format!("Archive/Box {i}"), &[]));
        }
        tree.push(folder("Health/Dentist", &[]));
        let dentist = tree.last().unwrap().id;
        // No word in common: only the embeddings find it.
        let context = context_with(tree, &words("tooth ache"), None, &[dentist, Uuid::now_v7()]);
        assert_eq!(context.ranked, [dentist]);
        assert!(context.find("Health/Dentist").is_some());
    }

    #[test]
    fn only_the_allowed_top_folders_and_no_hidden_ones() {
        let tree = vec![
            folder("Bills", &[]),
            folder("Private", &[]),
            folder("Bills/.cache", &[]),
        ];
        let context = context(tree, &[], Some(&["bills".to_string()]));
        let shown: Vec<String> = context.folders.iter().map(Folder::display).collect();
        assert_eq!(shown, ["Bills"]);
    }
}

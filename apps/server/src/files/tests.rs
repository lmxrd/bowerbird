//! The index, the reconciler, the operations and the journal, on real
//! folders in the temporary directory and an in-memory database.

use super::scan::Report;
use super::*;
use std::fs;

struct Setup {
    files: Arc<Files>,
    user: Uuid,
    root: PathBuf,
    dir: PathBuf,
}

impl Drop for Setup {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.dir);
    }
}

async fn setup() -> Setup {
    let dir = std::env::temp_dir().join(format!("organizer-files-{}", Uuid::now_v7()));
    let data_dir = dir.join("data");
    fs::create_dir_all(&data_dir).unwrap();
    let db = crate::db::open(":memory:").await.unwrap();
    let user = Uuid::now_v7();
    sqlx::query("INSERT INTO users (id, email, is_superadmin) VALUES (?1, 'a@example.com', 1)")
        .bind(user)
        .execute(db.writer())
        .await
        .unwrap();
    let files = Files::new(db, data_dir, None);
    let root = files.default_root(user);
    fs::create_dir_all(&root).unwrap();
    Setup {
        files,
        user,
        root,
        dir,
    }
}

impl Setup {
    async fn scan(&self) -> Report {
        self.files.scan(self.user).await.unwrap()
    }

    async fn hash(&self) {
        scan::hash_pending(
            self.files.db(),
            self.user,
            &crate::storage::Storage::disk(&self.root).unwrap(),
        )
        .await
        .unwrap();
    }

    /// The id of the node at a path ("a/b.txt").
    async fn id(&self, path: &str) -> Option<Uuid> {
        let mut id: Uuid =
            sqlx::query_scalar("SELECT id FROM nodes WHERE drive_id = ?1 AND parent_id IS NULL")
                .bind(self.user)
                .fetch_one(self.files.db().reader())
                .await
                .unwrap();
        for name in path.split('/') {
            id = sqlx::query_scalar("SELECT id FROM nodes WHERE parent_id = ?1 AND name = ?2")
                .bind(id)
                .bind(name)
                .fetch_optional(self.files.db().reader())
                .await
                .unwrap()?;
        }
        Some(id)
    }

    async fn count(&self) -> i64 {
        sqlx::query_scalar(
            "SELECT COUNT(*) FROM nodes WHERE drive_id = ?1 AND parent_id IS NOT NULL",
        )
        .bind(self.user)
        .fetch_one(self.files.db().reader())
        .await
        .unwrap()
    }

    async fn changes(&self) -> Vec<String> {
        sqlx::query_scalar("SELECT op FROM changes WHERE user_id = ?1 ORDER BY seq")
            .bind(self.user)
            .fetch_all(self.files.db().reader())
            .await
            .unwrap()
    }

    fn write(&self, path: &str, content: &str) {
        let path = self.root.join(path);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, content).unwrap();
    }
}

#[tokio::test]
async fn a_scan_indexes_the_tree_and_a_second_finds_nothing_new() {
    let s = setup().await;
    s.write("Documents/Taxes/2025.pdf", "pdf");
    s.write("Documents/notes.txt", "notes");
    s.write("photo.jpg", "jpeg");
    fs::create_dir_all(s.root.join("Empty")).unwrap();
    assert_eq!(
        s.scan().await,
        Report {
            created: 6,
            ..Report::default()
        }
    );
    assert_eq!(s.count().await, 6);
    assert_eq!(s.scan().await, Report::default());

    let node = s
        .files
        .node(s.user, s.id("photo.jpg").await.unwrap())
        .await
        .unwrap();
    assert_eq!(node.kind, Kind::File);
    assert_eq!(node.size, 4);
    assert_eq!(node.mime.as_deref(), Some("image/jpeg"));
    assert!(node.portable);
    assert_eq!(s.changes().await, vec!["create"; 6]);
}

#[tokio::test]
async fn changed_files_get_a_new_version_and_hash() {
    let s = setup().await;
    s.write("a.txt", "one");
    s.scan().await;
    s.hash().await;
    let id = s.id("a.txt").await.unwrap();
    let before = s.files.node(s.user, id).await.unwrap();

    s.write("a.txt", "one more");
    let report = s.scan().await;
    assert_eq!(report.updated, 1);
    let after = s.files.node(s.user, id).await.unwrap();
    assert_eq!(after.version, before.version + 1);
    assert_eq!(after.size, 8);
    let hash: Option<Vec<u8>> = sqlx::query_scalar("SELECT hash FROM nodes WHERE id = ?1")
        .bind(id)
        .fetch_one(s.files.db().reader())
        .await
        .unwrap();
    assert_eq!(hash.unwrap(), blake3::hash(b"one more").as_bytes().to_vec());
}

#[tokio::test]
async fn a_file_moved_outside_organizer_keeps_its_id() {
    let s = setup().await;
    s.write("Inbox/scan.pdf", "the scan");
    s.write("Inbox/other.pdf", "something else");
    fs::create_dir_all(s.root.join("Archive")).unwrap();
    s.scan().await;
    s.hash().await;
    let id = s.id("Inbox/scan.pdf").await.unwrap();

    // Moved and renamed, by Finder or a shell, into a new folder.
    fs::create_dir_all(s.root.join("Archive/2026")).unwrap();
    fs::rename(
        s.root.join("Inbox/scan.pdf"),
        s.root.join("Archive/2026/Scan 2026-11-06.pdf"),
    )
    .unwrap();
    let report = s.scan().await;
    assert_eq!(report.moved, 1);
    assert_eq!(report.created, 1); // the folder 2026
    assert_eq!(report.deleted, 0);
    assert_eq!(s.id("Archive/2026/Scan 2026-11-06.pdf").await, Some(id));
    assert_eq!(s.id("Inbox/scan.pdf").await, None);
    assert_eq!(s.changes().await.last().unwrap(), "move");

    // The whole folder moved: its files keep their ids, the folder itself
    // is new (folders are not matched by content).
    let other = s.id("Inbox/other.pdf").await.unwrap();
    fs::rename(s.root.join("Inbox"), s.root.join("Archive/Inbox")).unwrap();
    let report = s.scan().await;
    assert_eq!((report.moved, report.created, report.deleted), (1, 1, 1));
    assert_eq!(s.id("Archive/Inbox/other.pdf").await, Some(other));
}

#[tokio::test]
async fn deleted_files_and_folders_leave_the_index() {
    let s = setup().await;
    s.write("Old/a.txt", "a");
    s.write("Old/Deeper/b.txt", "b");
    s.write("keep.txt", "keep");
    s.scan().await;
    fs::remove_dir_all(s.root.join("Old")).unwrap();
    let report = s.scan().await;
    // Only the topmost of what vanished is deleted, and listed in the feed.
    assert_eq!(report.deleted, 1);
    assert_eq!(s.count().await, 1);
    assert_eq!(s.changes().await.last().unwrap(), "delete");
}

#[tokio::test]
async fn a_file_where_a_folder_was_is_a_new_node() {
    let s = setup().await;
    s.write("thing/inside.txt", "x");
    s.scan().await;
    let folder = s.id("thing").await.unwrap();
    fs::remove_dir_all(s.root.join("thing")).unwrap();
    s.write("thing", "now a file");
    let report = s.scan().await;
    assert_eq!((report.created, report.deleted), (1, 1));
    let file = s.id("thing").await.unwrap();
    assert_ne!(file, folder);
    assert_eq!(s.files.node(s.user, file).await.unwrap().kind, Kind::File);
}

#[tokio::test]
async fn an_empty_root_with_an_index_is_not_taken_for_deletions() {
    let s = setup().await;
    s.write("a.txt", "a");
    s.scan().await;
    fs::remove_file(s.root.join("a.txt")).unwrap();
    // Empty: like a share that is not mounted. Nothing changes.
    assert!(matches!(
        s.files.scan(s.user).await,
        Err(FileError::Unavailable(_))
    ));
    assert_eq!(s.count().await, 1);
    // Missing altogether: the same.
    fs::remove_dir_all(&s.root).unwrap();
    assert!(matches!(
        s.files.scan(s.user).await,
        Err(FileError::Unavailable(_))
    ));
}

#[tokio::test]
async fn names_made_outside_organizer_are_kept_and_flagged() {
    let s = setup().await;
    s.write("what?.txt", "x");
    s.write("Report.pdf", "1");
    s.write("report.pdf", "2");
    s.scan().await;
    // On a case-sensitive disk both exist; both are kept.
    if s.id("report.pdf").await.is_some() && s.id("Report.pdf").await.is_some() {
        assert_eq!(s.count().await, 3);
    }
    let odd = s
        .files
        .node(s.user, s.id("what?.txt").await.unwrap())
        .await
        .unwrap();
    assert!(!odd.portable);
}

#[tokio::test]
async fn folders_are_made_with_portable_names_that_do_not_clash() {
    let s = setup().await;
    s.write("Taxes/a.txt", "a");
    s.scan().await;
    let root = s.files.root_node(s.user).await.unwrap();

    let folder = s
        .files
        .create_folder(s.user, root.id, "  Re\u{301}sume\u{301}s ")
        .await
        .unwrap();
    assert_eq!(folder.name, "Résumés");
    assert!(s.root.join("Résumés").is_dir());
    assert_eq!(s.changes().await.last().unwrap(), "create");
    // A case-only clash, a bad name, a file as the parent.
    for (parent, name) in [
        (root.id, "taxes"),
        (root.id, "a:b"),
        (s.id("Taxes/a.txt").await.unwrap(), "x"),
    ] {
        assert!(
            s.files.create_folder(s.user, parent, name).await.is_err(),
            "{name}"
        );
    }
    // A scan finds nothing to change: the index already says it all.
    assert_eq!(s.scan().await, Report::default());
    // And nothing is left in the journal.
    let left: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM journal")
        .fetch_one(s.files.db().reader())
        .await
        .unwrap();
    assert_eq!(left, 0);
}

#[tokio::test]
async fn moves_check_the_version_the_target_and_the_name() {
    let s = setup().await;
    s.write("A/file.txt", "x");
    s.write("B/file.txt", "y");
    s.scan().await;
    let a = s.id("A").await.unwrap();
    let b = s.id("B").await.unwrap();
    let file = s
        .files
        .node(s.user, s.id("A/file.txt").await.unwrap())
        .await
        .unwrap();

    // A stale version is a conflict; so is a name taken in the target.
    assert!(matches!(
        s.files
            .move_node(s.user, file.id, Some(b), None, file.version + 1)
            .await,
        Err(FileError::Conflict(_))
    ));
    assert!(matches!(
        s.files
            .move_node(s.user, file.id, Some(b), None, file.version)
            .await,
        Err(FileError::Conflict(_))
    ));
    // A folder into itself.
    let a_node = s.files.node(s.user, a).await.unwrap();
    assert!(matches!(
        s.files
            .move_node(s.user, a, Some(a), None, a_node.version)
            .await,
        Err(FileError::Invalid(_))
    ));

    let moved = s
        .files
        .move_node(s.user, file.id, Some(b), Some("renamed.txt"), file.version)
        .await
        .unwrap();
    assert_eq!(moved.parent_id, Some(b));
    assert_eq!(moved.version, file.version + 1);
    assert!(s.root.join("B/renamed.txt").is_file());
    assert!(!s.root.join("A/file.txt").exists());
    // A rename that only changes the case is allowed.
    let renamed = s
        .files
        .move_node(s.user, file.id, None, Some("Renamed.txt"), moved.version)
        .await
        .unwrap();
    assert_eq!(renamed.name, "Renamed.txt");
    assert_eq!(s.scan().await, Report::default());
}

#[tokio::test]
async fn deletes_go_to_the_trash_and_come_back_with_their_id() {
    let s = setup().await;
    s.write("Projects/Plan/notes.txt", "notes");
    s.scan().await;
    let plan = s
        .files
        .node(s.user, s.id("Projects/Plan").await.unwrap())
        .await
        .unwrap();

    let item = s.files.trash(s.user, plan.id, plan.version).await.unwrap();
    assert_eq!(item.id, plan.id);
    assert_eq!(item.original_path, "Projects/Plan");
    assert!(!s.root.join("Projects/Plan").exists());
    assert!(
        s.files
            .tree(s.user)
            .await
            .unwrap()
            .trash_dir()
            .unwrap()
            .join(plan.id.to_string())
            .join("notes.txt")
            .is_file()
    );
    assert_eq!(s.id("Projects/Plan").await, None);
    assert_eq!(s.files.trash_items(s.user).await.unwrap().len(), 1);
    assert_eq!(s.scan().await, Report::default());

    // Back where it was, with its id and what was in it.
    let restored = s.files.restore(s.user, plan.id).await.unwrap();
    assert_eq!(restored.id, plan.id);
    assert_eq!(s.id("Projects/Plan").await, Some(plan.id));
    assert!(s.id("Projects/Plan/notes.txt").await.is_some());
    assert!(s.files.trash_items(s.user).await.unwrap().is_empty());

    // Its folder gone in the meantime: it comes back in the root.
    let plan = s.files.node(s.user, plan.id).await.unwrap();
    s.files.trash(s.user, plan.id, plan.version).await.unwrap();
    let projects = s
        .files
        .node(s.user, s.id("Projects").await.unwrap())
        .await
        .unwrap();
    s.files
        .trash(s.user, projects.id, projects.version)
        .await
        .unwrap();
    let restored = s.files.restore(s.user, plan.id).await.unwrap();
    assert_eq!(
        restored.parent_id,
        Some(s.files.root_node(s.user).await.unwrap().id)
    );
    assert!(s.root.join("Plan/notes.txt").is_file());

    // Emptied for good.
    s.files.purge(s.user, projects.id).await.unwrap();
    assert!(
        !s.files
            .tree(s.user)
            .await
            .unwrap()
            .trash_dir()
            .unwrap()
            .join(projects.id.to_string())
            .exists()
    );
    assert!(s.files.trash_items(s.user).await.unwrap().is_empty());
}

#[tokio::test]
async fn a_delete_interrupted_after_the_disk_is_recovered_into_the_trash() {
    let s = setup().await;
    s.write("doomed.txt", "x");
    s.scan().await;
    let id = s.id("doomed.txt").await.unwrap();
    // The process stopped after moving the file and before the index.
    let (from, to) = ("doomed.txt", crate::files::Tree::trash_path(id));
    journal::begin(
        &s.files,
        s.user,
        "trash",
        Some(id),
        Some(from),
        Some(&to),
        None,
    )
    .await
    .unwrap();
    fs::rename(s.root.join(from), s.root.join(&to)).unwrap();

    journal::recover(&s.files, None).await.unwrap();
    let trash = s.files.trash_items(s.user).await.unwrap();
    assert_eq!(trash.len(), 1);
    assert_eq!(trash[0].original_path, "doomed.txt");
    // The scan at start takes it out of the index... except that an empty
    // root is not trusted; with another file there it is.
    s.write("other.txt", "y");
    let report = s.scan().await;
    assert_eq!(report.deleted, 1);
    let left: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM journal")
        .fetch_one(s.files.db().reader())
        .await
        .unwrap();
    assert_eq!(left, 0);
}

#[tokio::test]
async fn a_files_root_must_be_an_existing_writable_folder_apart_from_the_data() {
    let s = setup().await;
    let elsewhere = s.dir.join("nas/share");
    fs::create_dir_all(&elsewhere).unwrap();
    assert!(s.files.check_root(&elsewhere).is_ok());
    for bad in [
        PathBuf::from("relative/path"),
        s.dir.join("missing"),
        s.files.data_dir().join("users"),
        s.dir.clone(),
    ] {
        assert!(s.files.check_root(&bad).is_err(), "{}", bad.display());
    }
}

#[tokio::test]
async fn a_new_files_root_is_indexed_from_scratch() {
    let s = setup().await;
    s.write("old.txt", "old");
    s.scan().await;
    let share = s.dir.join("share");
    fs::create_dir_all(share.join("Music")).unwrap();
    fs::write(share.join("Music/song.flac"), "la").unwrap();

    s.files.set_root(s.user, Some(&share)).await.unwrap();
    s.scan().await;
    assert_eq!(s.id("old.txt").await, None);
    assert!(s.id("Music/song.flac").await.is_some());
    // The old feed is gone with the old ids: clients list everything again.
    assert!(s.changes().await.iter().all(|op| op == "create"));
    let stored: Option<String> = sqlx::query_scalar("SELECT files_root FROM users WHERE id = ?1")
        .bind(s.user)
        .fetch_one(s.files.db().reader())
        .await
        .unwrap();
    assert_eq!(
        stored.map(PathBuf::from),
        Some(share.canonicalize().unwrap())
    );
}

#[tokio::test]
async fn files_dir_locks_the_root() {
    let s = setup().await;
    let share = s.dir.join("share");
    fs::create_dir_all(&share).unwrap();
    let files = Files::new(
        s.files.db().clone(),
        s.files.data_dir().to_path_buf(),
        Some(share.clone()),
    );
    let first = crate::files::StoredRoot {
        files_root: Some("/elsewhere".into()),
        owns_files_dir: true,
    };
    assert_eq!(files.root_for(s.user, &first), share);
    // Another user's is their own.
    let other = crate::files::StoredRoot {
        files_root: Some("/elsewhere".into()),
        owns_files_dir: false,
    };
    assert_eq!(
        files.root_for(s.user, &other),
        std::path::PathBuf::from("/elsewhere")
    );
    sqlx::query("UPDATE users SET owns_files_dir = 1 WHERE id = ?1")
        .bind(s.user)
        .execute(s.files.db().writer())
        .await
        .unwrap();
    assert!(matches!(
        files.set_root(s.user, None).await,
        Err(FileError::Invalid(_))
    ));
}

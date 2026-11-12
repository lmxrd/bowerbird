//! The engine against a real server, in-process (the backend crate, started
//! as its own tests start it), over a local socket and through a proxy the
//! tests can cut, with temporary folders on both sides.

use organizer::state::AppState;
use organizer_e2e_crypto::{self as crypto, contents, folder, keys, names as sealed};
use organizer_sync::local::Trash;
use organizer_sync::names::Naming;
use organizer_sync::remote::{Node, Remote};
use organizer_sync::{Config, Encryption, Engine, Mode, Phase};
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;
use uuid::Uuid;

struct Server {
    state: AppState,
    /// The user's tree on the server's disk (the default root under FILES_DIR).
    files: PathBuf,
    /// Through the proxy, which `offline` cuts.
    url: String,
    offline: Arc<AtomicBool>,
    token: String,
    user: Uuid,
    /// The user's session cookie (what only a session may do: encryption's
    /// set-up).
    session: String,
    /// The test's own folder: everything else goes under it.
    dir: PathBuf,
}

async fn server() -> Server {
    organizer::install_tls_provider();
    let dir = std::env::temp_dir().join(format!("organizer-sync-{}", Uuid::new_v4().simple()));
    let base = dir.join("server-files");
    std::fs::create_dir_all(&base).unwrap();
    let db = organizer::db::open(":memory:").await.unwrap();
    let state = AppState {
        oidc: Arc::new(organizer::oidc::Oidc::load(&db, organizer::oidc::Env::default()).await),
        files: organizer::files::Files::new(
            db.clone(),
            dir.join("data"),
            organizer::config::FilesDir::Local(base.clone()),
        ),
        db: db.clone(),
        trust_proxy: false,
        swagger_ui: false,
        limits: Arc::new(organizer::limits::Limits::with_bursts(1_000, 1_000, 1_000)),
        secure_cookies: false,
        public_url: None,
        challenge_key: axum_extra::extract::cookie::Key::generate(),
        totp_required: false,
        ai: Arc::new(organizer::ai::Ai::load(&db, organizer::ai::Env::default()).await),
        jobs: organizer::jobs::Jobs::new(db.clone(), organizer::handlers()),
        links_allow_private: false,
        vectors: organizer::vectors::open("turso::memory:").await.unwrap(),
        backup: Arc::new(
            organizer::backup::Backup::load(&db, organizer::backup::Env::default()).await,
        ),
        joining: organizer::users::Env::default(),
        push: Arc::new(
            organizer::push::Push::load(&db, Default::default(), false)
                .await
                .unwrap(),
        ),
    };
    state.jobs.start(state.clone()).await.unwrap();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let backend = listener.local_addr().unwrap();
    let app = organizer::app(state.clone());
    tokio::spawn(async move {
        axum::serve(
            listener,
            app.into_make_service_with_connect_info::<SocketAddr>(),
        )
        .await
        .unwrap();
    });
    let offline = Arc::new(AtomicBool::new(false));
    let url = proxy(backend, offline.clone()).await;

    let http = reqwest::Client::new();
    let (email, password) = ("anna@example.com", "a long enough password");
    let setup = http
        .post(format!("{url}/api/auth/setup"))
        .json(&serde_json::json!({ "email": email, "password": password }))
        .send()
        .await
        .unwrap();
    assert!(setup.status().is_success(), "setup: {}", setup.status());
    let session = cookie(&setup);
    let signed: serde_json::Value = http
        .post(format!("{url}/api/auth/device/password"))
        .json(&serde_json::json!({ "email": email, "password": password, "device_name": "Test" }))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let token = signed["token"]
        .as_str()
        .expect("a device token")
        .to_string();
    let user: Uuid = sqlx::query_scalar("SELECT id FROM users")
        .fetch_one(state.db.reader())
        .await
        .unwrap();
    // The user's tree: the default root under the files base.
    let files = state.files.default_root(user);
    std::fs::create_dir_all(&files).unwrap();
    Server {
        state,
        files,
        url,
        offline,
        token,
        user,
        session,
        dir,
    }
}

/// The session cookie an answer sets (`name=value`).
fn cookie(response: &reqwest::Response) -> String {
    response
        .headers()
        .get("set-cookie")
        .expect("a session cookie")
        .to_str()
        .unwrap()
        .split(';')
        .next()
        .unwrap()
        .to_string()
}

/// A proxy to the server that refuses connections, and breaks the open
/// ones, while `offline` is set: the server as a laptop sees it on a train.
async fn proxy(backend: SocketAddr, offline: Arc<AtomicBool>) -> String {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    tokio::spawn(async move {
        loop {
            let Ok((mut inbound, _)) = listener.accept().await else {
                return;
            };
            if offline.load(Ordering::SeqCst) {
                continue; // dropped
            }
            let offline = offline.clone();
            tokio::spawn(async move {
                let Ok(mut outbound) = tokio::net::TcpStream::connect(backend).await else {
                    return;
                };
                tokio::select! {
                    _ = tokio::io::copy_bidirectional(&mut inbound, &mut outbound) => {}
                    _ = async {
                        while !offline.load(Ordering::SeqCst) {
                            tokio::time::sleep(Duration::from_millis(20)).await;
                        }
                    } => {}
                }
            });
        }
    });
    url
}

impl Server {
    fn remote(&self) -> Remote {
        Remote::new(&self.url, &self.token).unwrap()
    }

    async fn root(&self) -> Node {
        self.remote().root().await.unwrap()
    }

    /// A node by its path from the root, if there.
    async fn at(&self, path: &str) -> Option<Node> {
        let mut node = self.root().await;
        for name in path.split('/') {
            let children = self.remote().children(node.id).await.unwrap();
            node = children.into_iter().find(|c| c.name == name)?;
        }
        Some(node)
    }

    async fn folder(&self, parent: Uuid, name: &str) -> Node {
        self.remote().create_folder(parent, name).await.unwrap()
    }

    /// A new file on the server, through the API.
    async fn upload(&self, parent: Uuid, name: &str, content: &[u8]) -> Node {
        let temp = self.dir.join(format!("upload-{}", Uuid::new_v4().simple()));
        std::fs::write(&temp, content).unwrap();
        let node = self
            .remote()
            .upload_new_plain(parent, name, &temp)
            .await
            .unwrap();
        std::fs::remove_file(temp).unwrap();
        node
    }

    async fn replace(&self, node: &Node, content: &[u8]) -> Node {
        let temp = self.dir.join(format!("upload-{}", Uuid::new_v4().simple()));
        std::fs::write(&temp, content).unwrap();
        let node = self
            .remote()
            .replace_plain(node.id, node.version, &temp)
            .await
            .unwrap();
        std::fs::remove_file(temp).unwrap();
        node
    }

    /// The content of a file of the server's tree, on its disk.
    fn read(&self, path: &str) -> Option<String> {
        std::fs::read_to_string(self.files.join(path)).ok()
    }

    /// The names in the server's trash.
    async fn trash(&self) -> Vec<String> {
        let items: Vec<serde_json::Value> = reqwest::Client::new()
            .get(format!("{}/api/trash", self.url))
            .bearer_auth(&self.token)
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap();
        items
            .iter()
            .map(|i| i["name"].as_str().unwrap_or_default().to_string())
            .collect()
    }

    /// An engine on this server with a fresh local folder, synced with the
    /// server's folder `remote` (made at the top when missing).
    async fn engine(&self, remote: &str) -> (Engine, PathBuf) {
        self.engine_with(remote, |_| {}).await
    }

    async fn engine_with(
        &self,
        remote: &str,
        tweak: impl FnOnce(&mut Config),
    ) -> (Engine, PathBuf) {
        self.engine_in(remote, Mode::Sync, tweak).await
    }

    async fn engine_in(
        &self,
        remote: &str,
        mode: Mode,
        tweak: impl FnOnce(&mut Config),
    ) -> (Engine, PathBuf) {
        let mut config = self.config();
        tweak(&mut config);
        let engine = Engine::open(config).await.unwrap();
        let root = self.root().await;
        let folder = match self.at(remote).await {
            Some(node) => node,
            None => self.folder(root.id, remote).await,
        };
        let local = self.dir.join("local").join(remote);
        engine.add_folder(folder.id, &local, mode).await.unwrap();
        (engine, local)
    }

    /// The tests' engine settings: a trash of its own, no waiting.
    fn config(&self) -> Config {
        let mut config = Config::new(&self.url, &self.token, "Laptop");
        config.trash = Trash::Folder(self.dir.join("local-trash"));
        config.settle = Duration::ZERO;
        config.debounce = Duration::from_millis(100);
        config
    }
}

fn write(path: &Path, content: &str) {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).unwrap();
    }
    std::fs::write(path, content).unwrap();
}

fn read(path: &Path) -> Option<String> {
    std::fs::read_to_string(path).ok()
}

/// The names in a local folder, sorted.
fn names(dir: &Path) -> Vec<String> {
    let mut names: Vec<String> = std::fs::read_dir(dir)
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    names.sort();
    names
}

/// A change made on disk is seen by the next modification-time check:
/// some file systems keep the time to the second.
async fn tick() {
    tokio::time::sleep(Duration::from_millis(1100)).await;
}

#[tokio::test]
async fn local_creates_changes_renames_moves_and_deletes_reach_the_server() {
    let server = server().await;
    let (engine, local) = server.engine("Docs").await;

    // Create: a file, and a folder with a file in it.
    write(&local.join("a.txt"), "one");
    write(&local.join("Taxes/2026.txt"), "taxes");
    engine.sync().await.unwrap();
    assert_eq!(server.read("Docs/a.txt").as_deref(), Some("one"));
    assert_eq!(server.read("Docs/Taxes/2026.txt").as_deref(), Some("taxes"));
    let a = server.at("Docs/a.txt").await.unwrap();
    let taxes = server.at("Docs/Taxes").await.unwrap();
    let file_2026 = server.at("Docs/Taxes/2026.txt").await.unwrap();

    // Change.
    tick().await;
    write(&local.join("a.txt"), "two, longer");
    engine.sync().await.unwrap();
    assert_eq!(server.read("Docs/a.txt").as_deref(), Some("two, longer"));

    // Rename: the same node, renamed.
    std::fs::rename(local.join("a.txt"), local.join("b.txt")).unwrap();
    engine.sync().await.unwrap();
    let b = server
        .at("Docs/b.txt")
        .await
        .expect("renamed on the server");
    assert_eq!(b.id, a.id);
    assert!(server.at("Docs/a.txt").await.is_none());

    // Move into a folder: the same node, moved.
    std::fs::rename(local.join("b.txt"), local.join("Taxes/b.txt")).unwrap();
    engine.sync().await.unwrap();
    assert_eq!(server.at("Docs/Taxes/b.txt").await.unwrap().id, a.id);

    // A folder renamed: moved as a whole, its files keep their ids (on
    // Windows, without inodes, the folder is made again and its files
    // moved into it, by their hash).
    std::fs::rename(local.join("Taxes"), local.join("Tax")).unwrap();
    engine.sync().await.unwrap();
    if cfg!(unix) {
        assert_eq!(server.at("Docs/Tax").await.unwrap().id, taxes.id);
    }
    assert_eq!(
        server.at("Docs/Tax/2026.txt").await.unwrap().id,
        file_2026.id
    );
    assert!(server.at("Docs/Taxes").await.is_none());

    // Delete: to the server's trash.
    std::fs::remove_file(local.join("Tax/b.txt")).unwrap();
    engine.sync().await.unwrap();
    assert!(server.at("Docs/Tax/b.txt").await.is_none());
    assert!(server.trash().await.contains(&"b.txt".to_string()));

    // A folder deleted, with what is in it.
    std::fs::remove_dir_all(local.join("Tax")).unwrap();
    engine.sync().await.unwrap();
    assert!(server.at("Docs/Tax").await.is_none());
    assert!(server.trash().await.contains(&"Tax".to_string()));
    assert_eq!(engine.status().borrow().phase, Phase::UpToDate);
}

#[tokio::test]
async fn remote_creates_changes_renames_moves_and_deletes_reach_the_disk() {
    let server = server().await;
    let (engine, local) = server.engine("Docs").await;
    engine.sync().await.unwrap();
    let docs = server.at("Docs").await.unwrap();

    // Create.
    let a = server.upload(docs.id, "a.txt", b"one").await;
    let taxes = server.folder(docs.id, "Taxes").await;
    server.upload(taxes.id, "2026.txt", b"taxes").await;
    engine.sync().await.unwrap();
    assert_eq!(read(&local.join("a.txt")).as_deref(), Some("one"));
    assert_eq!(
        read(&local.join("Taxes/2026.txt")).as_deref(),
        Some("taxes")
    );

    // Change.
    let a = server.replace(&a, b"two").await;
    engine.sync().await.unwrap();
    assert_eq!(read(&local.join("a.txt")).as_deref(), Some("two"));

    // Rename.
    let a = server
        .remote()
        .move_node(a.id, None, Some("b.txt"), a.version)
        .await
        .unwrap();
    engine.sync().await.unwrap();
    assert_eq!(names(&local), ["Taxes", "b.txt"]);

    // Move.
    server
        .remote()
        .move_node(a.id, Some(taxes.id), None, a.version)
        .await
        .unwrap();
    engine.sync().await.unwrap();
    assert_eq!(names(&local.join("Taxes")), ["2026.txt", "b.txt"]);
    assert_eq!(read(&local.join("Taxes/b.txt")).as_deref(), Some("two"));

    // A folder renamed: moved here as a whole, nothing downloaded again.
    let inode = local::inode(&local.join("Taxes/b.txt"));
    let taxes = server.at("Docs/Taxes").await.unwrap();
    server
        .remote()
        .move_node(taxes.id, None, Some("Tax"), taxes.version)
        .await
        .unwrap();
    engine.sync().await.unwrap();
    assert_eq!(names(&local), ["Tax"]);
    assert_eq!(local::inode(&local.join("Tax/b.txt")), inode);

    // Delete: to the local trash.
    let b = server.at("Docs/Tax/b.txt").await.unwrap();
    server.remote().delete(b.id, b.version).await.unwrap();
    engine.sync().await.unwrap();
    assert_eq!(names(&local.join("Tax")), ["2026.txt"]);
    let trashed = names(&server.dir.join("local-trash"));
    assert!(trashed.iter().any(|n| n.ends_with("-b.txt")), "{trashed:?}");

    // A folder deleted.
    let tax = server.at("Docs/Tax").await.unwrap();
    server.remote().delete(tax.id, tax.version).await.unwrap();
    engine.sync().await.unwrap();
    assert!(names(&local).is_empty());
}

mod local {
    pub fn inode(path: &std::path::Path) -> Option<i64> {
        organizer_sync::local::stat(path).and_then(|s| s.inode)
    }
}

#[tokio::test]
async fn a_change_on_both_sides_keeps_both_as_a_conflict_copy() {
    let server = server().await;
    let (engine, local) = server.engine("Docs").await;
    write(&local.join("Report.txt"), "first");
    engine.sync().await.unwrap();
    let report = server.at("Docs/Report.txt").await.unwrap();
    let mut events = engine.events();

    tick().await;
    write(&local.join("Report.txt"), "mine, local");
    server.replace(&report, b"theirs, on the server").await;
    engine.sync().await.unwrap();

    let date = jiff::Zoned::now().date().to_string();
    let copy = format!("Report (conflict, Laptop, {date}).txt");
    assert_eq!(
        read(&local.join("Report.txt")).as_deref(),
        Some("theirs, on the server")
    );
    assert_eq!(read(&local.join(&copy)).as_deref(), Some("mine, local"));
    // The copy is sent like any new file; the server's is untouched.
    assert_eq!(
        server.read(&format!("Docs/{copy}")).as_deref(),
        Some("mine, local")
    );
    assert_eq!(
        server.read("Docs/Report.txt").as_deref(),
        Some("theirs, on the server")
    );
    let status = engine.status().borrow().clone();
    assert_eq!(status.conflicts.len(), 1);
    assert_eq!(status.conflicts[0].copy, format!("Docs/{copy}"));
    assert!(matches!(
        events.try_recv(),
        Ok(organizer_sync::Event::Conflict { .. })
    ));
}

#[tokio::test]
async fn an_edit_wins_over_a_delete_on_the_other_side() {
    let server = server().await;
    let (engine, local) = server.engine("Docs").await;
    write(&local.join("kept-here.txt"), "a");
    write(&local.join("kept-there.txt"), "b");
    engine.sync().await.unwrap();

    // Changed here, deleted on the server: sent again.
    tick().await;
    write(&local.join("kept-here.txt"), "changed here");
    let here = server.at("Docs/kept-here.txt").await.unwrap();
    server.remote().delete(here.id, here.version).await.unwrap();
    // Changed on the server, deleted here: downloaded again.
    let there = server.at("Docs/kept-there.txt").await.unwrap();
    server.replace(&there, b"changed there").await;
    std::fs::remove_file(local.join("kept-there.txt")).unwrap();

    engine.sync().await.unwrap();
    assert_eq!(
        server.read("Docs/kept-here.txt").as_deref(),
        Some("changed here")
    );
    assert_eq!(
        read(&local.join("kept-there.txt")).as_deref(),
        Some("changed there")
    );
}

#[tokio::test]
async fn changes_wait_while_offline_and_go_once_the_server_is_back() {
    let server = server().await;
    let (engine, local) = server.engine("Docs").await;
    engine.sync().await.unwrap();

    server.offline.store(true, Ordering::SeqCst);
    // The proxy breaks the open connections too.
    tokio::time::sleep(Duration::from_millis(200)).await;
    write(&local.join("written-offline.txt"), "on the train");
    let result = engine.sync().await;
    assert!(
        matches!(result, Err(organizer_sync::Error::Offline(_))),
        "{result:?}"
    );
    assert_eq!(engine.status().borrow().phase, Phase::Offline);
    assert!(server.read("Docs/written-offline.txt").is_none());

    server.offline.store(false, Ordering::SeqCst);
    engine.sync().await.unwrap();
    assert_eq!(
        server.read("Docs/written-offline.txt").as_deref(),
        Some("on the train")
    );
    assert_eq!(engine.status().borrow().phase, Phase::UpToDate);
}

#[tokio::test]
async fn a_cursor_too_old_lists_everything_again() {
    let server = server().await;
    let (engine, local) = server.engine("Docs").await;
    let docs = server.at("Docs").await.unwrap();
    server.upload(docs.id, "before.txt", b"1").await;
    engine.sync().await.unwrap();

    let after = server.upload(docs.id, "after.txt", b"2").await;
    let before = server.at("Docs/before.txt").await.unwrap();
    server
        .remote()
        .delete(before.id, before.version)
        .await
        .unwrap();
    // The feed pruned past the engine's cursor: a 410.
    sqlx::query("UPDATE users SET feed_floor = feed_floor + 1000000")
        .execute(server.state.db.writer())
        .await
        .unwrap();
    assert!(matches!(
        server.remote().changes(Some("0.1")).await,
        Err(organizer_sync::Error::Gone)
    ));

    engine.sync().await.unwrap();
    assert_eq!(names(&local), ["after.txt"]);
    assert_eq!(server.at("Docs/after.txt").await.unwrap().id, after.id);
}

// The server's names are made on its disk, which on Windows refuses them.
#[cfg(not(windows))]
#[tokio::test]
async fn names_this_system_refuses_get_safe_local_names_that_round_trip() {
    let server = server().await;
    std::fs::create_dir_all(server.files.join("Docs")).unwrap();
    std::fs::write(server.files.join("Docs/a:b.txt"), "colon").unwrap();
    // Case-only twins, where the server's disk can hold them (Linux).
    let twins = !organizer_sync::names::case_insensitive(&server.files);
    std::fs::write(server.files.join("Docs/Notes.txt"), "upper").unwrap();
    if twins {
        std::fs::write(server.files.join("Docs/notes.txt"), "lower").unwrap();
    }
    server.state.files.scan(server.user).await.unwrap();
    let (engine, local) = server
        .engine_with("Docs", |c| {
            c.naming = Some(Naming {
                windows: true,
                case_insensitive: true,
            })
        })
        .await;
    engine.sync().await.unwrap();
    let mut expected = vec!["Notes.txt", "a_b.txt"];
    if twins {
        expected.push("notes (2).txt");
        assert_eq!(read(&local.join("notes (2).txt")).as_deref(), Some("lower"));
    }
    assert_eq!(names(&local), expected);

    // A change of the mapped file goes to the server's name, not a rename.
    tick().await;
    write(&local.join("a_b.txt"), "colon, changed");
    engine.sync().await.unwrap();
    assert_eq!(
        server.read("Docs/a:b.txt").as_deref(),
        Some("colon, changed")
    );
    assert!(server.at("Docs/a_b.txt").await.is_none());

    // The server's rename comes down mapped again.
    let node = server.at("Docs/a:b.txt").await.unwrap();
    server
        .remote()
        .move_node(node.id, None, Some("c-d.txt"), node.version)
        .await
        .unwrap();
    engine.sync().await.unwrap();
    expected[1] = "c-d.txt";
    assert_eq!(names(&local), expected);
}

#[tokio::test]
async fn an_interrupted_resumable_upload_carries_on_where_it_broke_off() {
    let server = server().await;
    let (engine, local) = server.engine("Docs").await;
    engine.sync().await.unwrap();
    // Over the plain uploads' 4 MB: resumable.
    let content: String = (0..6 * 1024 * 1024 / 16)
        .map(|i| format!("{i:015}\n"))
        .collect();
    write(&local.join("big.txt"), &content);

    engine.break_uploads_after(Some(1024 * 1024));
    let result = engine.sync().await;
    assert!(
        matches!(result, Err(organizer_sync::Error::Offline(_))),
        "{result:?}"
    );
    let url = engine
        .upload_in_progress(1, "big.txt")
        .await
        .expect("the upload is kept");
    let offset = engine.remote().upload_offset(&url).await.unwrap();
    assert!(offset.is_some(), "the server has the upload");
    assert!(server.read("Docs/big.txt").is_none());

    engine.break_uploads_after(None);
    engine.sync().await.unwrap();
    let error = engine.status().borrow().error.clone();
    assert!(
        server.read("Docs/big.txt") == Some(content),
        "the whole file is there ({error:?})"
    );
    assert!(engine.upload_in_progress(1, "big.txt").await.is_none());
}

#[tokio::test]
async fn the_drop_box_folder_sends_its_files_to_the_drop_box() {
    let server = server().await;
    let drop_box = server.dir.join("Drop box");
    let (engine, _) = server
        .engine_with("Docs", |c| c.drop_box = Some(drop_box.clone()))
        .await;
    write(&drop_box.join("invoice.pdf"), "an invoice");
    write(&drop_box.join(".DS_Store"), "");
    engine.sync().await.unwrap();

    assert_eq!(names(&drop_box), [".DS_Store"]);
    let items: Vec<serde_json::Value> = reqwest::Client::new()
        .get(format!("{}/api/drop-box/items", server.url))
        .bearer_auth(&server.token)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(items.len(), 1);
    assert_eq!(items[0]["name"], "invoice.pdf");
    let recent = engine.status().borrow().recent.clone();
    assert_eq!(recent[0].kind, organizer_sync::ActivityKind::DropBox);
}

#[tokio::test]
async fn running_it_follows_the_watcher_and_the_live_events_and_pauses() {
    let server = server().await;
    let (engine, local) = server.engine("Docs").await;
    engine.start();
    let docs = server.at("Docs").await.unwrap();

    // A local file: the watcher wakes a pass.
    write(&local.join("watched.txt"), "seen");
    wait_for(|| server.read("Docs/watched.txt").is_some()).await;

    // A file on the server: the live event wakes a pass.
    server.upload(docs.id, "pushed.txt", b"pushed").await;
    wait_for(|| local.join("pushed.txt").exists()).await;

    // Paused: nothing goes, until resumed.
    engine.pause();
    write(&local.join("paused.txt"), "waits");
    tokio::time::sleep(Duration::from_millis(1500)).await;
    assert!(server.read("Docs/paused.txt").is_none());
    assert_eq!(engine.status().borrow().phase, Phase::Paused);
    engine.resume();
    wait_for(|| server.read("Docs/paused.txt").is_some()).await;
    engine.stop();
}

async fn wait_for(mut done: impl FnMut() -> bool) {
    for _ in 0..200 {
        if done() {
            return;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    panic!("timed out");
}

// --- On-demand folders (docs/M5B_DESIGN.md) --------------------------------

use organizer_sync::on_demand::{
    self, Fetch, Fetcher, Identity, PinAction, Placeholder, PlaceholderState, Placeholders,
};
use std::collections::HashMap;
use std::sync::Mutex;

/// Placeholders as the tests see them, where there are none (on Windows
/// they are the Cloud Files API's): an empty file stands for one not
/// downloaded, and the fake keeps what the system would (its identity,
/// size, pin, and the file as it was when last in sync, since the system
/// clears "in sync" on a write). Hydrating fetches the bytes as the system
/// would, through the engine's `Fetcher`.
struct Fake {
    fetcher: Fetcher,
    files: Mutex<HashMap<PathBuf, FakeFile>>,
    calls: Mutex<Vec<String>>,
}

#[derive(Clone)]
struct FakeFile {
    identity: Identity,
    size: u64,
    hydrated: bool,
    pin: PinAction,
    synced: Option<(i64, i64)>,
}

fn stamp(path: &Path) -> Option<(i64, i64)> {
    organizer_sync::local::stat(path).map(|s| (s.size, s.mtime_ns))
}

impl Fake {
    fn new(server: &Server) -> Arc<Self> {
        Arc::new(Fake {
            fetcher: Fetcher::new(server.remote(), tokio::runtime::Handle::current()),
            files: Mutex::new(HashMap::new()),
            calls: Mutex::new(Vec::new()),
        })
    }

    fn file(&self, path: &Path) -> Option<FakeFile> {
        self.files.lock().unwrap().get(path).cloned()
    }

    fn pin(&self, path: &Path, pin: PinAction) {
        self.files.lock().unwrap().get_mut(path).unwrap().pin = pin;
    }

    fn called(&self, call: &str) -> bool {
        self.calls.lock().unwrap().iter().any(|c| c == call)
    }

    fn call(&self, what: &str, path: &Path) {
        let name = path.file_name().unwrap().to_string_lossy();
        self.calls.lock().unwrap().push(format!("{what} {name}"));
    }
}

impl Placeholders for Fake {
    fn connect(&self, root: &Path, _: &str, _: &str) -> std::io::Result<()> {
        self.call("connect", root);
        Ok(())
    }

    fn release(&self, root: &Path, _: &str) -> std::io::Result<()> {
        self.call("release", root);
        Ok(())
    }

    fn create(&self, path: &Path, file: &Placeholder) -> std::io::Result<()> {
        self.call("create", path);
        std::fs::write(path, b"")?;
        self.files.lock().unwrap().insert(
            path.to_path_buf(),
            FakeFile {
                identity: file.identity,
                size: file.size,
                hydrated: false,
                pin: PinAction::Nothing,
                synced: stamp(path),
            },
        );
        Ok(())
    }

    fn state(&self, path: &Path) -> std::io::Result<Option<PlaceholderState>> {
        if !path.exists() {
            return Ok(None);
        }
        Ok(self.file(path).map(|f| PlaceholderState {
            identity: Some(f.identity),
            in_sync: f.synced == stamp(path),
            hydrated: f.hydrated,
            pin: f.pin,
        }))
    }

    fn update(&self, path: &Path, file: &Placeholder) -> std::io::Result<()> {
        self.call("update", path);
        std::fs::write(path, b"")?;
        let mut files = self.files.lock().unwrap();
        let f = files.get_mut(path).unwrap();
        f.identity = file.identity;
        f.size = file.size;
        f.hydrated = false;
        f.synced = stamp(path);
        Ok(())
    }

    fn adopt(&self, path: &Path, file: &Placeholder) -> std::io::Result<()> {
        self.call("adopt", path);
        self.files.lock().unwrap().insert(
            path.to_path_buf(),
            FakeFile {
                identity: file.identity,
                size: file.size,
                hydrated: true,
                pin: PinAction::Nothing,
                synced: stamp(path),
            },
        );
        Ok(())
    }

    fn hydrate(&self, path: &Path) -> std::io::Result<()> {
        self.call("hydrate", path);
        let file = self.file(path).unwrap();
        let mut bytes = Vec::new();
        self.fetcher
            .fetch(file.identity, 0, file.size, &mut |chunk| {
                bytes.extend_from_slice(chunk);
                Ok(())
            })?;
        std::fs::write(path, bytes)?;
        let mut files = self.files.lock().unwrap();
        let f = files.get_mut(path).unwrap();
        f.hydrated = true;
        f.pin = PinAction::Nothing;
        f.synced = stamp(path);
        Ok(())
    }

    fn dehydrate(&self, path: &Path) -> std::io::Result<()> {
        self.call("dehydrate", path);
        std::fs::write(path, b"")?;
        let mut files = self.files.lock().unwrap();
        let f = files.get_mut(path).unwrap();
        f.hydrated = false;
        f.pin = PinAction::Nothing;
        f.synced = stamp(path);
        Ok(())
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn an_on_demand_folder_shows_the_servers_files_and_downloads_them_when_asked() {
    let server = server().await;
    let docs = server.folder(server.root().await.id, "Docs").await;
    let a = server.upload(docs.id, "a.txt", b"alpha").await;
    let sub = server.folder(docs.id, "Sub").await;
    let b = server.upload(sub.id, "b.txt", b"beta").await;
    let fake = Fake::new(&server);
    let provider = on_demand::Provider(fake.clone());
    let (engine, local) = server
        .engine_in("Docs", Mode::OnDemand, |c| c.placeholders = Some(provider))
        .await;
    assert!(fake.called("connect Docs"));
    engine.sync().await.unwrap();

    // Placeholders of the server's files, of their version and size,
    // nothing downloaded.
    let (a_path, b_path) = (local.join("a.txt"), local.join("Sub/b.txt"));
    assert_eq!(read(&a_path).as_deref(), Some(""));
    let placeholder = fake.file(&a_path).unwrap();
    assert_eq!(placeholder.identity, on_demand::identity(a.id, a.version));
    assert_eq!(placeholder.size, 5);
    assert!(!placeholder.hydrated);
    assert_eq!(
        fake.file(&b_path).unwrap().identity,
        on_demand::identity(b.id, b.version)
    );

    // Changed on the server: the placeholder shows the new version, still
    // not downloaded.
    let a2 = server.replace(&a, b"alpha, again").await;
    engine.sync().await.unwrap();
    let placeholder = fake.file(&a_path).unwrap();
    assert_eq!(placeholder.identity, on_demand::identity(a.id, a2.version));
    assert_eq!(placeholder.size, 12);
    assert!(fake.called("update a.txt"));
    assert_eq!(read(&a_path).as_deref(), Some(""));

    // "Always keep on this device": downloaded, at that version.
    fake.pin(&a_path, PinAction::Hydrate);
    engine.sync().await.unwrap();
    assert_eq!(read(&a_path).as_deref(), Some("alpha, again"));
    assert!(fake.file(&a_path).unwrap().hydrated);

    // Edited here: sent at its version, and in sync at the new one.
    tick().await;
    write(&a_path, "edited here");
    engine.sync().await.unwrap();
    assert_eq!(server.read("Docs/a.txt").as_deref(), Some("edited here"));
    let a3 = server.at("Docs/a.txt").await.unwrap();
    assert_eq!(
        fake.file(&a_path).unwrap().identity,
        on_demand::identity(a.id, a3.version)
    );

    // A new file here: sent, then the server's placeholder.
    write(&local.join("c.txt"), "new here");
    engine.sync().await.unwrap();
    let c = server.at("Docs/c.txt").await.expect("sent");
    assert_eq!(
        fake.file(&local.join("c.txt")).unwrap().identity,
        on_demand::identity(c.id, c.version)
    );

    // "Free up space": freed, and nothing is sent for it.
    fake.pin(&a_path, PinAction::Dehydrate);
    engine.sync().await.unwrap();
    assert!(fake.called("dehydrate a.txt"));
    engine.sync().await.unwrap();
    assert_eq!(server.read("Docs/a.txt").as_deref(), Some("edited here"));
    assert_eq!(server.at("Docs/a.txt").await.unwrap().version, a3.version);

    // Renamed here while not downloaded: moved on the server, never
    // downloaded to be recognised.
    std::fs::rename(&a_path, local.join("d.txt")).unwrap();
    let moved = fake.file(&a_path).unwrap();
    fake.files
        .lock()
        .unwrap()
        .insert(local.join("d.txt"), moved);
    engine.sync().await.unwrap();
    assert_eq!(server.at("Docs/d.txt").await.unwrap().id, a.id);
    assert_eq!(server.read("Docs/d.txt").as_deref(), Some("edited here"));

    // Deleted on the server: a placeholder not downloaded goes for good
    // (the server's trash has it), not to the local trash.
    server.remote().delete(b.id, b.version).await.unwrap();
    engine.sync().await.unwrap();
    assert!(!b_path.exists());
    assert!(!server.dir.join("local-trash").exists());

    // Removed: the folder is released.
    let folder = engine.folders().await.unwrap()[0].id;
    engine.remove_folder(folder).await.unwrap();
    assert!(fake.called("release Docs"));
}

#[tokio::test(flavor = "multi_thread")]
async fn a_fetch_sends_a_range_of_the_version_a_placeholder_shows() {
    let server = server().await;
    let root = server.root().await;
    let content: Vec<u8> = (0..10_000u32).map(|i| (i % 251) as u8).collect();
    let node = server.upload(root.id, "big.bin", &content).await;
    let fetcher = Arc::new(Fetcher::new(
        server.remote(),
        tokio::runtime::Handle::current(),
    ));

    let fetch = |identity: Identity, offset: u64, length: u64| {
        let fetcher = fetcher.clone();
        tokio::task::spawn_blocking(move || {
            let mut bytes = Vec::new();
            fetcher
                .fetch(identity, offset, length, &mut |chunk| {
                    bytes.extend_from_slice(chunk);
                    Ok(())
                })
                .map(|()| bytes)
        })
    };
    let identity = on_demand::identity(node.id, node.version);
    let bytes = fetch(identity, 4096, 4096).await.unwrap().unwrap();
    assert_eq!(bytes, content[4096..8192]);
    let bytes = fetch(identity, 8192, 1808).await.unwrap().unwrap();
    assert_eq!(bytes, content[8192..]);

    // Changed on the server since: not the placeholder's bytes.
    server.replace(&node, b"another").await;
    assert!(fetch(identity, 0, 4096).await.unwrap().is_err());
}

// Not on Windows: there the engine always has the system's placeholders
// (Cloud Files, when the config gives none) and keeps the folder itself, as
// the tests with the fake placeholders check.
#[cfg(not(windows))]
#[tokio::test]
async fn an_on_demand_folder_without_placeholders_is_left_to_the_system() {
    // macOS: the File Provider extension keeps it; the engine only lists it.
    let server = server().await;
    let docs = server.folder(server.root().await.id, "Docs").await;
    server.upload(docs.id, "a.txt", b"alpha").await;
    let (engine, local) = server.engine_in("Docs", Mode::OnDemand, |_| {}).await;
    engine.sync().await.unwrap();
    assert!(!local.exists());
    let folders = engine.folders().await.unwrap();
    assert_eq!(folders[0].mode(), Mode::OnDemand);
    let status = engine.status().borrow().clone();
    assert_eq!((status.phase, status.error), (Phase::UpToDate, None));
}

// --- End-to-end-encrypted folders ------------------------------------------

const PASSPHRASE: &str = "correct horse battery staple";

impl Server {
    /// A JSON request: with a session cookie when given, else the token.
    async fn json(
        &self,
        method: reqwest::Method,
        path: &str,
        session: Option<&str>,
        body: Option<serde_json::Value>,
    ) -> serde_json::Value {
        let mut request = reqwest::Client::new().request(method, format!("{}/api{path}", self.url));
        request = match session {
            Some(session) => request.header("cookie", session),
            None => request.bearer_auth(&self.token),
        };
        if let Some(body) = body {
            request = request.json(&body);
        }
        let response = request.send().await.unwrap();
        let status = response.status();
        let text = response.text().await.unwrap();
        assert!(status.is_success(), "{path}: {status} {text}");
        serde_json::from_str(&text).unwrap_or_default()
    }

    /// Encryption set up for the user of `session`, with real keys: their
    /// identity.
    async fn set_up_encryption(&self, session: &str) -> keys::Identity {
        let identity = keys::Identity::generate();
        let salt = crypto::random::<16>();
        let wrapped =
            keys::wrap_with_passphrase(&identity.private, PASSPHRASE, &salt, keys::MIN_ITERATIONS);
        let recovery = keys::wrap_private(
            &keys::recovery_wrapping_key(&crypto::random_32()),
            &identity.private,
            &crypto::random(),
        );
        self.json(
            reqwest::Method::POST,
            "/encryption",
            Some(session),
            Some(serde_json::json!({
                "public_key": crypto::b64(&identity.public),
                "passphrase": {
                    "salt": crypto::b64(&salt),
                    "iterations": keys::MIN_ITERATIONS,
                    "wrapped": crypto::b64(&wrapped),
                },
                "recovery_wrapped": crypto::b64(&recovery),
            })),
        )
        .await;
        identity
    }

    /// A new encrypted folder (without a key yet).
    async fn encrypted_folder(&self, parent: Uuid, name: &str) -> Uuid {
        let made = self
            .json(
                reqwest::Method::POST,
                &format!("/files/{parent}/folders"),
                None,
                Some(serde_json::json!({ "name": name, "encrypted": true })),
            )
            .await;
        made["id"].as_str().unwrap().parse().unwrap()
    }

    /// A folder key of a generation, posted for one member.
    async fn post_key(
        &self,
        top: Uuid,
        user: Uuid,
        generation: u32,
        key: &[u8; 32],
        public: &[u8; 32],
    ) {
        let wrapped = folder::wrap(key, top, generation, public).unwrap();
        self.json(
            reqwest::Method::POST,
            &format!("/encryption/folders/{top}/keys"),
            None,
            Some(serde_json::json!({ "keys": [{
                "user_id": user, "generation": generation, "wrapped": crypto::b64(&wrapped)
            }] })),
        )
        .await;
    }

    /// A file put in an encrypted folder as a device would: its name
    /// sealed, its contents encrypted.
    async fn sealed_file(
        &self,
        key: (&[u8; 32], u32),
        parent: Uuid,
        name: &str,
        content: &[u8],
    ) -> Node {
        let name = sealed::encrypt(key.0, key.1, parent, name).unwrap();
        self.upload(parent, &name, &contents::encrypt(key.0, key.1, content))
            .await
    }

    async fn sealed_folder(&self, key: (&[u8; 32], u32), parent: Uuid, name: &str) -> Node {
        let name = sealed::encrypt(key.0, key.1, parent, name).unwrap();
        self.folder(parent, &name).await
    }

    /// The bytes of a file as the server keeps them.
    async fn content(&self, id: Uuid) -> Vec<u8> {
        reqwest::Client::new()
            .get(format!("{}/api/files/{id}/content", self.url))
            .bearer_auth(&self.token)
            .send()
            .await
            .unwrap()
            .bytes()
            .await
            .unwrap()
            .to_vec()
    }

    /// What is in an encrypted folder, by its decrypted names (none of
    /// which may be the plain one).
    async fn sealed_children(&self, parent: Uuid, keys: &[[u8; 32]]) -> Vec<(String, Node)> {
        let mut out = Vec::new();
        for node in self.remote().children(parent).await.unwrap() {
            let name = keys
                .iter()
                .find_map(|k| sealed::decrypt(k, parent, &node.name).ok())
                .unwrap_or_else(|| panic!("{} is not sealed for its folder", node.name));
            assert_ne!(name, node.name);
            out.push((name, node));
        }
        out.sort_by(|a, b| a.0.cmp(&b.0));
        out
    }
}

fn names_of(children: &[(String, Node)]) -> Vec<&str> {
    children.iter().map(|(n, _)| n.as_str()).collect()
}

fn node_named<'a>(children: &'a [(String, Node)], name: &str) -> &'a Node {
    &children.iter().find(|(n, _)| n == name).unwrap().1
}

/// Bytes that are not text, over several segments of 64 KiB.
fn pattern(len: usize) -> Vec<u8> {
    (0..len).map(|i| (i % 251) as u8).collect()
}

#[tokio::test]
async fn an_encrypted_folder_is_synced_decrypted_here_and_sealed_on_the_server() {
    let server = server().await;
    let me = server.set_up_encryption(&server.session).await;
    let (engine, local) = server.engine("Docs").await;
    let docs = server.at("Docs").await.unwrap();
    let private = server.encrypted_folder(docs.id, "Private").await;
    let (k1, k2) = (crypto::random_32(), crypto::random_32());
    server
        .post_key(private, server.user, 1, &k1, &me.public)
        .await;
    let sub = server.sealed_folder((&k1, 1), private, "Sub").await;
    let a = server
        .sealed_file((&k1, 1), private, "a.txt", b"hello")
        .await;
    let big = pattern(200_000);
    server.sealed_file((&k1, 1), sub.id, "big.bin", &big).await;
    // A second generation, as after a member left: written after with it.
    server
        .post_key(private, server.user, 2, &k2, &me.public)
        .await;
    let second = server
        .sealed_file((&k2, 2), sub.id, "second.txt", b"of the second key")
        .await;
    write(&local.join("plain.txt"), "plain");

    // Locked: the encrypted folder is left alone, the rest syncs.
    engine.sync().await.unwrap();
    assert_eq!(names(&local), ["plain.txt"]);
    assert_eq!(
        engine.status().borrow().encryption,
        Encryption {
            set_up: true,
            unlocked: false,
            skipped: 1
        }
    );
    assert!(engine.unlock("not the passphrase").await.is_err());

    // Unlocked: down, decrypted.
    engine.unlock(PASSPHRASE).await.unwrap();
    engine.sync().await.unwrap();
    assert_eq!(names(&local.join("Private")), ["Sub", "a.txt"]);
    assert_eq!(read(&local.join("Private/a.txt")).as_deref(), Some("hello"));
    assert_eq!(
        std::fs::read(local.join("Private/Sub/big.bin")).unwrap(),
        big
    );
    assert_eq!(
        read(&local.join("Private/Sub/second.txt")).as_deref(),
        Some("of the second key")
    );
    assert_eq!(engine.status().borrow().encryption.skipped, 0);

    // A rename on the server (sealed again) reaches the disk.
    let renamed = sealed::encrypt(&k2, 2, sub.id, "2nd.txt").unwrap();
    server
        .remote()
        .move_node(second.id, None, Some(&renamed), second.version)
        .await
        .unwrap();
    engine.sync().await.unwrap();
    assert_eq!(names(&local.join("Private/Sub")), ["2nd.txt", "big.bin"]);

    // Up: a new file, a new folder with a file, a rename, a change; all
    // sealed with the newest key.
    write(&local.join("Private/new.txt"), "secret");
    write(&local.join("Private/Made/inner.txt"), "inside");
    std::fs::rename(
        local.join("Private/a.txt"),
        local.join("Private/renamed.txt"),
    )
    .unwrap();
    engine.sync().await.unwrap();
    tick().await;
    write(&local.join("Private/renamed.txt"), "hello, changed");
    engine.sync().await.unwrap();
    let keys = [k1, k2];
    let open =
        |bytes: &[u8]| contents::decrypt(|g| keys.get(g as usize - 1).copied(), bytes).unwrap();
    let children = server.sealed_children(private, &keys).await;
    assert_eq!(
        names_of(&children),
        ["Made", "Sub", "new.txt", "renamed.txt"]
    );
    let new = node_named(&children, "new.txt");
    assert_eq!(sealed::generation(&new.name).unwrap(), 2);
    let bytes = server.content(new.id).await;
    assert!(!bytes.windows(6).any(|w| w == b"secret"));
    assert_eq!(open(&bytes), b"secret");
    let renamed = node_named(&children, "renamed.txt");
    assert_eq!(renamed.id, a.id, "renamed, not sent again");
    assert_eq!(open(&server.content(renamed.id).await), b"hello, changed");
    let made = node_named(&children, "Made").id;
    let inner = server.sealed_children(made, &keys).await;
    assert_eq!(names_of(&inner), ["inner.txt"]);
    assert_eq!(open(&server.content(inner[0].1.id).await), b"inside");

    // Moved inside it: the same node, its name sealed for its new parent.
    std::fs::rename(
        local.join("Private/new.txt"),
        local.join("Private/Made/new.txt"),
    )
    .unwrap();
    engine.sync().await.unwrap();
    let inner = server.sealed_children(made, &keys).await;
    assert_eq!(names_of(&inner), ["inner.txt", "new.txt"]);
    assert_eq!(node_named(&inner, "new.txt").id, new.id);

    // Moved across the encrypted folder's edge, which the server refuses:
    // sent in, sealed, and the plain one to the trash.
    std::fs::rename(local.join("plain.txt"), local.join("Private/Sub/plain.txt")).unwrap();
    engine.sync().await.unwrap();
    assert!(server.at("Docs/plain.txt").await.is_none());
    assert!(server.trash().await.contains(&"plain.txt".to_string()));
    let in_sub = server.sealed_children(sub.id, &keys).await;
    assert_eq!(names_of(&in_sub), ["2nd.txt", "big.bin", "plain.txt"]);
    let plain = node_named(&in_sub, "plain.txt");
    assert_eq!(open(&server.content(plain.id).await), b"plain");
    // And everything agreed on: nothing left to do.
    engine.sync().await.unwrap();
    assert_eq!(
        names(&local.join("Private/Sub")),
        ["2nd.txt", "big.bin", "plain.txt"]
    );
    let status = engine.status().borrow().clone();
    assert_eq!(status.phase, Phase::UpToDate, "{:?}", status.error);
}

#[tokio::test]
async fn a_locked_encrypted_folder_is_left_alone_on_both_sides() {
    let server = server().await;
    let me = server.set_up_encryption(&server.session).await;
    let root = server.root().await;
    let private = server.encrypted_folder(root.id, "Private").await;
    let k1 = crypto::random_32();
    server
        .post_key(private, server.user, 1, &k1, &me.public)
        .await;
    server.sealed_file((&k1, 1), private, "a.txt", b"one").await;
    server.sealed_file((&k1, 1), private, "b.txt", b"two").await;
    // The encrypted folder itself synced, and a plain one beside it.
    let (engine, local) = server.engine("Private").await;
    let other = server.folder(root.id, "Other").await;
    let other_local = server.dir.join("local/Other");
    engine
        .add_folder(other.id, &other_local, Mode::Sync)
        .await
        .unwrap();
    engine.unlock(PASSPHRASE).await.unwrap();
    engine.sync().await.unwrap();
    assert_eq!(names(&local), ["a.txt", "b.txt"]);

    // Locked: changes on either side wait, nothing is deleted.
    engine.set_private_key(None);
    std::fs::remove_file(local.join("a.txt")).unwrap();
    write(&local.join("local.txt"), "waits");
    server
        .sealed_file((&k1, 1), private, "remote.txt", b"comes later")
        .await;
    write(&other_local.join("outside.txt"), "plain");
    engine.sync().await.unwrap();
    let on_server = server.sealed_children(private, &[k1]).await;
    assert_eq!(names_of(&on_server), ["a.txt", "b.txt", "remote.txt"]);
    assert_eq!(names(&local), ["b.txt", "local.txt"]);
    assert_eq!(server.read("Other/outside.txt").as_deref(), Some("plain"));
    let encryption = engine.status().borrow().encryption.clone();
    assert!(
        !encryption.unlocked && encryption.skipped == 1,
        "{encryption:?}"
    );

    // Unlocked again: both sides catch up.
    engine.unlock(PASSPHRASE).await.unwrap();
    engine.sync().await.unwrap();
    let on_server = server.sealed_children(private, &[k1]).await;
    assert_eq!(names_of(&on_server), ["b.txt", "local.txt", "remote.txt"]);
    assert_eq!(names(&local), ["b.txt", "local.txt", "remote.txt"]);
    assert_eq!(
        read(&local.join("remote.txt")).as_deref(),
        Some("comes later")
    );
}

#[tokio::test]
async fn an_unlocked_device_wraps_the_keys_for_members_and_makes_new_ones() {
    let server = server().await;
    let me = server.set_up_encryption(&server.session).await;
    let (engine, _) = server.engine("Docs").await;
    let docs = server.at("Docs").await.unwrap();
    let shared = server.encrypted_folder(docs.id, "Shared").await;
    let k1 = crypto::random_32();
    server
        .post_key(shared, server.user, 1, &k1, &me.public)
        .await;
    // A folder without a key yet: its owner's device makes the first.
    let fresh = server.encrypted_folder(docs.id, "Fresh").await;

    // Bea, invited, sets up encryption; the folder is shared with her.
    // Made on the server's database: the superadmin's endpoint would want
    // a second factor confirmed first, which is not what this tests.
    organizer::users::make_invite(
        &server.state.db,
        Some("bea@example.com"),
        false,
        server.user,
    )
    .await
    .unwrap();
    let token: String = sqlx::query_scalar("SELECT token FROM invites WHERE email = ?1")
        .bind("bea@example.com")
        .fetch_one(server.state.db.reader())
        .await
        .unwrap();
    let accepted = reqwest::Client::new()
        .post(format!("{}/api/auth/invite/{token}", server.url))
        .json(&serde_json::json!({
            "email": "bea@example.com",
            "password": "correct horse battery"
        }))
        .send()
        .await
        .unwrap();
    assert!(accepted.status().is_success(), "{}", accepted.status());
    let bea_session = cookie(&accepted);
    let bea = server
        .json(
            reqwest::Method::GET,
            "/auth/session",
            Some(&bea_session),
            None,
        )
        .await["user"]["id"]
        .as_str()
        .unwrap()
        .to_string();
    let bea_keys = server.set_up_encryption(&bea_session).await;
    server
        .json(
            reqwest::Method::POST,
            &format!("/files/{shared}/shares"),
            None,
            Some(serde_json::json!({ "user_id": bea, "permissions": {} })),
        )
        .await;
    let keys_of = |top: Uuid| format!("/encryption/folders/{top}");
    let get = reqwest::Method::GET;

    // Locked, this device can do nothing for them.
    engine.sync().await.unwrap();
    let hers = server
        .json(get.clone(), &keys_of(shared), Some(&bea_session), None)
        .await;
    assert_eq!(hers["keys"], serde_json::json!([]));

    // Unlocked, it wraps generation 1 for her, and makes Fresh's first.
    engine.unlock(PASSPHRASE).await.unwrap();
    engine.sync().await.unwrap();
    let hers = server
        .json(get.clone(), &keys_of(shared), Some(&bea_session), None)
        .await;
    assert_eq!(hers["keys"][0]["generation"], 1);
    let wrapped = crypto::unb64(hers["keys"][0]["wrapped"].as_str().unwrap()).unwrap();
    assert_eq!(
        folder::unwrap(&wrapped, shared, 1, &bea_keys.private).unwrap(),
        k1
    );
    let mine = server.json(get.clone(), &keys_of(fresh), None, None).await;
    assert_eq!(mine["generation"], 1);
    assert_eq!(mine["needs_new_key"], false);
    let wrapped = crypto::unb64(mine["keys"][0]["wrapped"].as_str().unwrap()).unwrap();
    assert!(folder::unwrap(&wrapped, fresh, 1, &me.private).is_ok());
    let pending = server.json(get, "/encryption/pending", None, None).await;
    assert_eq!(pending, serde_json::json!([]));
}

#[tokio::test]
async fn an_interrupted_encrypted_upload_carries_on_with_the_same_bytes() {
    let server = server().await;
    let me = server.set_up_encryption(&server.session).await;
    let root = server.root().await;
    let private = server.encrypted_folder(root.id, "Private").await;
    let k1 = crypto::random_32();
    server
        .post_key(private, server.user, 1, &k1, &me.public)
        .await;
    let (engine, local) = server.engine("Private").await;
    engine.unlock(PASSPHRASE).await.unwrap();
    engine.sync().await.unwrap();
    // Over the plain uploads' 4 MB: resumable, sized by its encrypted bytes.
    let big = pattern(5 * 1024 * 1024);
    std::fs::write(local.join("big.bin"), &big).unwrap();

    engine.break_uploads_after(Some(1024 * 1024));
    let result = engine.sync().await;
    assert!(
        matches!(result, Err(organizer_sync::Error::Offline(_))),
        "{result:?}"
    );
    assert!(engine.upload_in_progress(1, "big.bin").await.is_some());

    engine.break_uploads_after(None);
    engine.sync().await.unwrap();
    let on_server = server.sealed_children(private, &[k1]).await;
    assert_eq!(names_of(&on_server), ["big.bin"]);
    let bytes = server.content(on_server[0].1.id).await;
    assert_eq!(
        bytes.len() as u64,
        contents::encrypted_size(big.len() as u64)
    );
    assert_eq!(contents::decrypt(|_| Some(k1), &bytes).unwrap(), big);
}

// --- Encrypted folders and on demand ---------------------------------------

#[tokio::test(flavor = "multi_thread")]
async fn an_encrypted_folder_is_never_on_demand() {
    let server = server().await;
    let me = server.set_up_encryption(&server.session).await;
    let root = server.root().await;
    let private = server.encrypted_folder(root.id, "Private").await;
    let k1 = crypto::random_32();
    server
        .post_key(private, server.user, 1, &k1, &me.public)
        .await;
    let sub = server.sealed_folder((&k1, 1), private, "Sub").await;
    let fake = Fake::new(&server);
    let mut config = Config::new(&server.url, &server.token, "Laptop");
    config.placeholders = Some(on_demand::Provider(fake.clone()));
    let engine = Engine::open(config).await.unwrap();
    engine.unlock(PASSPHRASE).await.unwrap();

    // The encrypted folder, or one inside it: refused, nothing made.
    for (id, name) in [(private, "Private"), (sub.id, "Sub")] {
        let local = server.dir.join("local").join(name);
        let refused = engine.add_folder(id, &local, Mode::OnDemand).await;
        assert!(
            matches!(&refused, Err(e) if e.to_string() == organizer_sync::engine::ENCRYPTED_ON_DEMAND),
            "{refused:?}"
        );
        assert!(!local.exists());
    }
    assert!(engine.folders().await.unwrap().is_empty());
    assert!(!fake.called("connect Private"));

    // Synced in full, it is.
    let local = server.dir.join("local/Private");
    engine
        .add_folder(private, &local, Mode::Sync)
        .await
        .unwrap();
    engine.sync().await.unwrap();
    assert_eq!(names(&local), ["Sub"]);
}

#[tokio::test(flavor = "multi_thread")]
async fn the_encrypted_folders_in_an_on_demand_folder_are_left_out_of_it() {
    let server = server().await;
    let me = server.set_up_encryption(&server.session).await;
    let docs = server.folder(server.root().await.id, "Docs").await;
    let a = server.upload(docs.id, "a.txt", b"alpha").await;
    let private = server.encrypted_folder(docs.id, "Private").await;
    let k1 = crypto::random_32();
    server
        .post_key(private, server.user, 1, &k1, &me.public)
        .await;
    server
        .sealed_file((&k1, 1), private, "secret.txt", b"sealed")
        .await;
    let fake = Fake::new(&server);
    let provider = on_demand::Provider(fake.clone());
    let (engine, local) = server
        .engine_in("Docs", Mode::OnDemand, |c| c.placeholders = Some(provider))
        .await;
    // Unlocked or not, its placeholders could only hold the sealed bytes.
    engine.unlock(PASSPHRASE).await.unwrap();
    engine.sync().await.unwrap();
    assert_eq!(names(&local), ["a.txt"]);
    assert_eq!(
        fake.file(&local.join("a.txt")).unwrap().identity,
        on_demand::identity(a.id, a.version)
    );
    // Not counted as locked: it is left out by design.
    assert_eq!(engine.status().borrow().encryption.skipped, 0);

    // Something made here under its name is left alone too, on both sides.
    write(&local.join("Private/mine.txt"), "here");
    engine.sync().await.unwrap();
    assert_eq!(
        read(&local.join("Private/mine.txt")).as_deref(),
        Some("here")
    );
    let on_server = server.sealed_children(private, &[k1]).await;
    assert_eq!(names_of(&on_server), ["secret.txt"]);
    assert_eq!(engine.status().borrow().error, None);
}

// --- Drives (docs/M7A_DESIGN.md) -------------------------------------------

impl Server {
    /// A shared drive the user is a member of (not managing it, so that
    /// they may be taken out of it), made on the server's side: the
    /// superadmin's endpoint would want a second factor confirmed first,
    /// which is not what these tests are about. Its id and its root.
    async fn shared_drive(&self, name: &str) -> (Uuid, Node) {
        let files = &self.state.files;
        let id = files
            .create_shared_drive(name, None, self.user)
            .await
            .unwrap();
        files.root_node(id).await.unwrap();
        files.set_member(id, self.user, false).await.unwrap();
        let drive = self.remote().drive(&id.to_string()).await.unwrap();
        let root = self
            .remote()
            .node(drive.root_id.expect("the drive's folder"))
            .await
            .unwrap();
        (id, root)
    }

    /// An engine with no folder yet.
    async fn bare_engine(&self) -> Engine {
        Engine::open(self.config()).await.unwrap()
    }

    /// The names in a folder of the server, sorted.
    async fn names_in(&self, folder: Uuid) -> Vec<String> {
        let mut names: Vec<String> = self
            .remote()
            .children(folder)
            .await
            .unwrap()
            .into_iter()
            .map(|n| n.name)
            .collect();
        names.sort();
        names
    }
}

#[tokio::test]
async fn a_shared_drive_is_synced_and_the_other_drives_changes_are_let_go() {
    let server = server().await;
    let (team, team_root) = server.shared_drive("Team").await;
    let (_, other_root) = server.shared_drive("Other").await;
    let engine = server.bare_engine().await;
    let local = server.dir.join("local/Team");
    engine
        .add_folder(team_root.id, &local, Mode::Sync)
        .await
        .unwrap();
    assert_eq!(
        engine.folders().await.unwrap()[0].drive_id,
        Some(team.to_string())
    );
    engine.sync().await.unwrap();

    // Changes in the drive arrive; those in another drive, in My drive,
    // and a drive joined since, are let go.
    server.upload(team_root.id, "a.txt", b"team").await;
    server.upload(other_root.id, "b.txt", b"other").await;
    server
        .upload(server.root().await.id, "c.txt", b"mine")
        .await;
    let (_, later_root) = server.shared_drive("Later").await;
    server.upload(later_root.id, "d.txt", b"later").await;
    engine.sync().await.unwrap();
    assert_eq!(names(&local), ["a.txt"]);
    assert_eq!(read(&local.join("a.txt")).as_deref(), Some("team"));

    // What is made here goes to the drive; a `.organizer` at its root is
    // the server's own name there, never sent (one further down is).
    write(&local.join("e.txt"), "here");
    write(&local.join(".organizer/trash/x.txt"), "not sent");
    write(&local.join("Sub/.organizer/y.txt"), "sent");
    engine.sync().await.unwrap();
    assert_eq!(
        server.names_in(team_root.id).await,
        ["Sub", "a.txt", "e.txt"]
    );
    let children = server.remote().children(team_root.id).await.unwrap();
    let sub = children.iter().find(|n| n.name == "Sub").unwrap();
    assert_eq!(server.names_in(sub.id).await, [".organizer"]);
    assert_eq!(
        read(&local.join(".organizer/trash/x.txt")).as_deref(),
        Some("not sent")
    );
    let status = engine.status().borrow().clone();
    assert_eq!((status.phase, status.error), (Phase::UpToDate, None));
    assert!(status.unavailable.is_empty());
}

#[tokio::test]
async fn leaving_a_synced_drive_stops_it_and_keeps_the_local_files() {
    let server = server().await;
    let (team, team_root) = server.shared_drive("Team").await;
    server.upload(team_root.id, "a.txt", b"team").await;
    let engine = server.bare_engine().await;
    let local = server.dir.join("local/Team");
    let folder = engine
        .add_folder(team_root.id, &local, Mode::Sync)
        .await
        .unwrap();
    engine.sync().await.unwrap();
    assert_eq!(names(&local), ["a.txt"]);

    // Taken out of the drive: unavailable, nothing deleted here.
    let files = &server.state.files;
    files.remove_member(team, server.user).await.unwrap();
    engine.sync().await.unwrap();
    assert_eq!(names(&local), ["a.txt"]);
    assert!(!server.dir.join("local-trash").exists());
    let status = engine.status().borrow().clone();
    assert_eq!(status.unavailable, [folder.id]);
    assert_eq!((status.phase, status.error), (Phase::UpToDate, None));

    // A change made here meanwhile waits; back in the drive, it is sent,
    // and what was there is kept.
    write(&local.join("b.txt"), "made meanwhile");
    engine.sync().await.unwrap();
    files.set_member(team, server.user, false).await.unwrap();
    engine.sync().await.unwrap();
    assert_eq!(server.names_in(team_root.id).await, ["a.txt", "b.txt"]);
    assert_eq!(names(&local), ["a.txt", "b.txt"]);
    assert!(engine.status().borrow().unavailable.is_empty());

    // The drive deleted: the same, its files stay here.
    files.delete_drive(team).await.unwrap();
    engine.sync().await.unwrap();
    assert_eq!(names(&local), ["a.txt", "b.txt"]);
    assert!(!server.dir.join("local-trash").exists());
    assert_eq!(engine.status().borrow().unavailable, [folder.id]);

    // An engine that starts afresh finds it gone (404) the same way.
    let fresh = server.bare_engine().await;
    let elsewhere = server.dir.join("local/Team again");
    write(&elsewhere.join("kept.txt"), "kept");
    let again = fresh
        .add_folder(team_root.id, &elsewhere, Mode::Sync)
        .await
        .unwrap();
    assert_eq!(again.drive_id, None);
    fresh.sync().await.unwrap();
    assert_eq!(names(&elsewhere), ["kept.txt"]);
    assert_eq!(fresh.status().borrow().unavailable, [again.id]);

    // Removed: its files stay.
    engine.remove_folder(folder.id).await.unwrap();
    assert_eq!(names(&local), ["a.txt", "b.txt"]);
    assert!(engine.status().borrow().unavailable.is_empty());
}

#[tokio::test]
async fn a_move_between_two_drives_synced_is_a_delete_and_a_create() {
    // The server moves nothing between drives (400): the engine, which
    // keeps each synced folder on its own, trashes it in one and sends it
    // to the other.
    let server = server().await;
    let (_, team_root) = server.shared_drive("Team").await;
    let (engine, docs) = server.engine("Docs").await;
    let team = server.dir.join("local/Team");
    engine
        .add_folder(team_root.id, &team, Mode::Sync)
        .await
        .unwrap();
    write(&docs.join("a.txt"), "moving");
    engine.sync().await.unwrap();
    let a = server.at("Docs/a.txt").await.unwrap();

    std::fs::rename(docs.join("a.txt"), team.join("a.txt")).unwrap();
    engine.sync().await.unwrap();
    assert!(server.at("Docs/a.txt").await.is_none());
    assert!(server.trash().await.contains(&"a.txt".to_string()));
    let moved = server.remote().children(team_root.id).await.unwrap();
    assert_eq!(moved.len(), 1);
    assert_eq!(moved[0].name, "a.txt");
    assert_ne!(moved[0].id, a.id);
    assert_eq!(read(&team.join("a.txt")).as_deref(), Some("moving"));
    let status = engine.status().borrow().clone();
    assert_eq!((status.phase, status.error), (Phase::UpToDate, None));
}

#[tokio::test(flavor = "multi_thread")]
async fn a_shared_drive_can_be_on_demand() {
    let server = server().await;
    let (_, team_root) = server.shared_drive("Team").await;
    let a = server.upload(team_root.id, "a.txt", b"alpha").await;
    let fake = Fake::new(&server);
    let mut config = server.config();
    config.placeholders = Some(on_demand::Provider(fake.clone()));
    let engine = Engine::open(config).await.unwrap();
    let local = server.dir.join("local/Team");
    engine
        .add_folder(team_root.id, &local, Mode::OnDemand)
        .await
        .unwrap();
    assert!(fake.called("connect Team"));
    engine.sync().await.unwrap();
    let placeholder = fake.file(&local.join("a.txt")).unwrap();
    assert_eq!(placeholder.identity, on_demand::identity(a.id, a.version));
    assert!(!placeholder.hydrated);

    // Its `.organizer` is left alone here too.
    write(&local.join(".organizer/x.txt"), "not sent");
    engine.sync().await.unwrap();
    assert_eq!(server.names_in(team_root.id).await, ["a.txt"]);
    assert_eq!(engine.status().borrow().error, None);
}

// --- Roles (docs/M7B_DESIGN.md §7) -----------------------------------------

use organizer::files::grants::Grantee;
use organizer::files::ops::WriteTarget;
use organizer::files::roles::{GrantRow, Role};
use organizer_sync::{ActivityKind, Held, NotSynced};

impl Server {
    /// A shared drive (with no manager: the server made it) on which the
    /// user has `role`: its id, its root and the user's grant.
    async fn drive_as(&self, name: &str, role: Role) -> (Uuid, Node, GrantRow) {
        let files = &self.state.files;
        let id = files
            .create_shared_drive(name, None, self.user)
            .await
            .unwrap();
        files.root_node(id).await.unwrap();
        let grant = files
            .grant(id, None, Grantee::User(self.user), role, self.user)
            .await
            .unwrap();
        let drive = self.remote().drive(&id.to_string()).await.unwrap();
        let root = self
            .remote()
            .node(drive.root_id.expect("the drive's folder"))
            .await
            .unwrap();
        (id, root, grant)
    }

    /// A file written on the server's side, by nobody: someone else's
    /// (upload only does not see it).
    async fn theirs(&self, drive: Uuid, parent: Uuid, name: &str, content: &str) -> Uuid {
        self.write_theirs(
            drive,
            WriteTarget::New {
                parent,
                name: name.into(),
            },
            content,
        )
        .await
    }

    async fn write_theirs(&self, drive: Uuid, target: WriteTarget, content: &str) -> Uuid {
        let temp = self.dir.join(format!("theirs-{}", Uuid::new_v4().simple()));
        std::fs::write(&temp, content).unwrap();
        let node = self
            .state
            .files
            .write_file(drive, target, &temp)
            .await
            .unwrap();
        node.id
    }
}

/// What the status lists as not synced, sorted.
fn not_synced(engine: &Engine) -> Vec<NotSynced> {
    let mut held = engine.status().borrow().not_synced.clone();
    held.sort_by(|a, b| a.path.cmp(&b.path));
    held
}

fn held(path: &str, why: Held) -> NotSynced {
    NotSynced {
        path: path.into(),
        why,
        reason: None,
    }
}

#[tokio::test]
async fn a_viewers_folder_is_read_only_here() {
    let server = server().await;
    let (team, root, grant) = server.drive_as("Team", Role::Viewer).await;
    let a = server.theirs(team, root.id, "a.txt", "a").await;
    let d = server.theirs(team, root.id, "d.txt", "d").await;
    let sub = server
        .state
        .files
        .create_folder(team, root.id, "Sub")
        .await
        .unwrap();
    server.theirs(team, sub.id, "b.txt", "b").await;
    let engine = server.bare_engine().await;
    let local = server.dir.join("local/Team");
    engine
        .add_folder(root.id, &local, Mode::Sync)
        .await
        .unwrap();
    engine.sync().await.unwrap();
    assert_eq!(names(&local), ["Sub", "a.txt", "d.txt"]);
    assert_eq!(read(&local.join("Sub/b.txt")).as_deref(), Some("b"));

    // Made, changed, renamed here: kept as it is, nothing sent, and told.
    tick().await;
    write(&local.join("new.txt"), "new");
    write(&local.join("New/x.txt"), "x");
    write(&local.join("a.txt"), "changed here");
    std::fs::rename(local.join("Sub/b.txt"), local.join("Sub/c.txt")).unwrap();
    // Deleted here: the server's copy comes back.
    std::fs::remove_file(local.join("d.txt")).unwrap();
    engine.sync().await.unwrap();
    assert_eq!(server.names_in(root.id).await, ["Sub", "a.txt", "d.txt"]);
    assert_eq!(server.names_in(sub.id).await, ["b.txt"]);
    assert_eq!(server.content(a).await, b"a");
    assert_eq!(names(&local), ["New", "Sub", "a.txt", "d.txt", "new.txt"]);
    assert_eq!(read(&local.join("a.txt")).as_deref(), Some("changed here"));
    assert_eq!(read(&local.join("New/x.txt")).as_deref(), Some("x"));
    assert_eq!(names(&local.join("Sub")), ["c.txt"]);
    assert_eq!(read(&local.join("d.txt")).as_deref(), Some("d"));
    assert_eq!(
        not_synced(&engine),
        [
            held("Team/New", Held::ViewOnly),
            held("Team/Sub/c.txt", Held::ViewOnly),
            held("Team/a.txt", Held::ViewOnly),
            held("Team/new.txt", Held::ViewOnly),
        ]
    );
    let status = engine.status().borrow().clone();
    assert_eq!((status.phase, status.error), (Phase::UpToDate, None));
    assert!(
        status
            .recent
            .iter()
            .any(|a| a.kind == ActivityKind::Restore)
    );
    assert!(!server.dir.join("local-trash").exists());

    // The server's changes still arrive.
    let version = server.remote().node(d).await.unwrap().version;
    let target = WriteTarget::Replace {
        id: d,
        if_version: version,
        source: None,
    };
    server.write_theirs(team, target, "d, again").await;
    engine.sync().await.unwrap();
    assert_eq!(read(&local.join("d.txt")).as_deref(), Some("d, again"));

    // An editor now (which no change of the feed tells): what was kept
    // here is sent.
    server
        .state
        .files
        .change_grant(grant.id, Role::Editor)
        .await
        .unwrap();
    engine.sync().await.unwrap();
    assert_eq!(
        server.names_in(root.id).await,
        ["New", "Sub", "a.txt", "d.txt", "new.txt"]
    );
    assert_eq!(server.names_in(sub.id).await, ["c.txt"]);
    assert_eq!(server.content(a).await, b"changed here");
    assert!(not_synced(&engine).is_empty());
    let status = engine.status().borrow().clone();
    assert_eq!((status.phase, status.error), (Phase::UpToDate, None));
}

#[tokio::test]
async fn upload_only_sends_new_files_and_keeps_the_rest_here() {
    let server = server().await;
    let (drop, root, grant) = server.drive_as("Drop", Role::Editor).await;
    server.theirs(drop, root.id, "theirs.txt", "theirs").await;
    let engine = server.bare_engine().await;
    let local = server.dir.join("local/Drop");
    engine
        .add_folder(root.id, &local, Mode::Sync)
        .await
        .unwrap();
    engine.sync().await.unwrap();
    assert_eq!(names(&local), ["theirs.txt"]);

    // Upload only now: the others' files are no longer seen, and stay here
    // (never a mass delete), not synced.
    server
        .state
        .files
        .change_grant(grant.id, Role::UploadOnly)
        .await
        .unwrap();
    engine.sync().await.unwrap();
    assert_eq!(names(&local), ["theirs.txt"]);
    assert!(!server.dir.join("local-trash").exists());
    assert_eq!(not_synced(&engine), [held("Drop/theirs.txt", Held::Hidden)]);

    // What is made here goes, folders too.
    write(&local.join("mine.txt"), "mine");
    write(&local.join("Folder/f.txt"), "f");
    engine.sync().await.unwrap();
    assert_eq!(server.names_in(root.id).await, ["Folder", "mine.txt"]);
    let children = server.remote().children(root.id).await.unwrap();
    let mine = children.iter().find(|n| n.name == "mine.txt").unwrap().id;

    // A rename or a change of what was sent is not: kept here, and told.
    tick().await;
    std::fs::rename(local.join("mine.txt"), local.join("renamed.txt")).unwrap();
    write(&local.join("Folder/f.txt"), "f, changed");
    engine.sync().await.unwrap();
    assert_eq!(server.names_in(root.id).await, ["Folder", "mine.txt"]);
    assert_eq!(server.content(mine).await, b"mine");
    assert_eq!(names(&local), ["Folder", "renamed.txt", "theirs.txt"]);
    assert_eq!(
        not_synced(&engine),
        [
            held("Drop/Folder/f.txt", Held::UploadOnly),
            held("Drop/renamed.txt", Held::UploadOnly),
            held("Drop/theirs.txt", Held::Hidden),
        ]
    );

    // Nor is a delete: what was sent comes back.
    std::fs::remove_dir_all(local.join("Folder")).unwrap();
    engine.sync().await.unwrap();
    assert_eq!(server.names_in(root.id).await, ["Folder", "mine.txt"]);
    assert_eq!(read(&local.join("Folder/f.txt")).as_deref(), Some("f"));
    let status = engine.status().borrow().clone();
    assert_eq!((status.phase, status.error), (Phase::UpToDate, None));
}

#[tokio::test]
async fn a_folder_no_longer_seen_is_kept_here_until_it_is_seen_again() {
    let server = server().await;
    let (team, root, _) = server.drive_as("Team", Role::Editor).await;
    let a = server.theirs(team, root.id, "a.txt", "a").await;
    let secret = server
        .state
        .files
        .create_folder(team, root.id, "Secret")
        .await
        .unwrap();
    let s = server.theirs(team, secret.id, "s.txt", "s").await;
    let engine = server.bare_engine().await;
    let local = server.dir.join("local/Team");
    engine
        .add_folder(root.id, &local, Mode::Sync)
        .await
        .unwrap();
    engine.sync().await.unwrap();
    assert_eq!(read(&local.join("Secret/s.txt")).as_deref(), Some("s"));

    // Limited access on it, without a grant for the user: it disappears
    // from the feed as a delete. Its files, and what was changed in it
    // here, stay; nothing is sent.
    server
        .state
        .files
        .set_limited(secret.id, true)
        .await
        .unwrap();
    tick().await;
    write(&local.join("Secret/s.txt"), "s, changed here");
    write(&local.join("Secret/new.txt"), "new");
    engine.sync().await.unwrap();
    assert_eq!(names(&local), ["Secret", "a.txt"]);
    assert_eq!(names(&local.join("Secret")), ["new.txt", "s.txt"]);
    assert!(!server.dir.join("local-trash").exists());
    assert_eq!(server.names_in(root.id).await, ["a.txt"]);
    assert_eq!(not_synced(&engine), [held("Team/Secret", Held::Hidden)]);
    let status = engine.status().borrow().clone();
    assert_eq!((status.phase, status.error), (Phase::UpToDate, None));

    // A delete is still one: to the trash here.
    let version = server.remote().node(a).await.unwrap().version;
    server.remote().delete(a, version).await.unwrap();
    engine.sync().await.unwrap();
    assert_eq!(names(&local), ["Secret"]);
    assert!(server.dir.join("local-trash").exists());

    // Seen again: what waited here is sent.
    server
        .state
        .files
        .set_limited(secret.id, false)
        .await
        .unwrap();
    engine.sync().await.unwrap();
    assert_eq!(server.names_in(secret.id).await, ["new.txt", "s.txt"]);
    assert_eq!(server.content(s).await, b"s, changed here");
    assert!(not_synced(&engine).is_empty());
}

// --- Storage backends (docs/M7C_DESIGN.md) ---------------------------------

use organizer_sync::{Error, Waiting};

impl Server {
    /// The drive put on the user's own storage with private credentials
    /// that no session unlocked: locked ("Locked until … signs in"), its
    /// storage unavailable. Made on the server's side, as these tests have
    /// no S3 (the backend's own tests cover one's stand-in).
    async fn lock_drive(&self, drive: Uuid) {
        let backend = Uuid::new_v4();
        sqlx::query(
            "INSERT INTO backends
               (id, owner_id, kind, name, endpoint, bucket, access_key, sealed_secret)
             VALUES (?1, ?2, 's3', 'Bucket', 'https://s3.example.com', 'bucket', 'AKIA', 'x')",
        )
        .bind(backend)
        .bind(self.user)
        .execute(self.state.db.writer())
        .await
        .unwrap();
        self.set_backend(drive, Some(backend)).await;
    }

    /// The drive back on the server's disk, where its files still are.
    async fn unlock_drive(&self, drive: Uuid) {
        self.set_backend(drive, None).await;
    }

    async fn set_backend(&self, drive: Uuid, backend: Option<Uuid>) {
        sqlx::query("UPDATE drives SET backend_id = ?2 WHERE id = ?1")
            .bind(drive)
            .bind(backend)
            .execute(self.state.db.writer())
            .await
            .unwrap();
        // Its tree is opened anew, on its new storage.
        self.state.files.forget(drive).await;
    }

    /// The drive being moved (to the server's disk, its own: the job does
    /// not run, so the move lasts until `end_move`): read-only meanwhile.
    async fn start_move(&self, drive: Uuid) {
        sqlx::query("INSERT INTO drive_moves (drive_id) VALUES (?1)")
            .bind(drive)
            .execute(self.state.db.writer())
            .await
            .unwrap();
        self.state.files.forget(drive).await;
    }

    async fn end_move(&self, drive: Uuid) {
        sqlx::query("DELETE FROM drive_moves WHERE drive_id = ?1")
            .bind(drive)
            .execute(self.state.db.writer())
            .await
            .unwrap();
        self.state.files.forget(drive).await;
    }
}

fn held_because(path: &str, why: Held, reason: &str) -> NotSynced {
    NotSynced {
        path: path.into(),
        why,
        reason: Some(reason.into()),
    }
}

#[tokio::test]
async fn a_drive_whose_storage_is_unavailable_waits_and_keeps_local_changes() {
    let server = server().await;
    let (team, root) = server.shared_drive("Team").await;
    let a = server.upload(root.id, "a.txt", b"a").await;
    server.upload(root.id, "b.txt", b"b").await;
    server.upload(root.id, "d.txt", b"d").await;
    let engine = server.bare_engine().await;
    let local = server.dir.join("local/Team");
    let folder = engine
        .add_folder(root.id, &local, Mode::Sync)
        .await
        .unwrap();
    engine.sync().await.unwrap();
    assert_eq!(names(&local), ["a.txt", "b.txt", "d.txt"]);

    // Locked: the server says why, for the drive and for its files.
    server.lock_drive(team).await;
    let reason = "Locked until anna@example.com signs in";
    let drive = server.remote().drive(&team.to_string()).await.unwrap();
    assert!(!drive.available);
    assert_eq!(
        drive.unavailable,
        Some(organizer_sync::api::types::Unavailable::Locked)
    );
    assert_eq!(drive.unavailable_reason.as_deref(), Some(reason));
    let temp = server.dir.join("refused.txt");
    std::fs::write(&temp, "x").unwrap();
    match server
        .remote()
        .upload_new_plain(root.id, "x.txt", &temp)
        .await
    {
        Err(Error::StorageUnavailable(said)) => assert_eq!(said, reason),
        other => panic!("not refused as unavailable: {other:?}"),
    }

    // Made, changed and deleted here meanwhile: nothing sent, nothing
    // brought back or deleted here, and each listed with the reason.
    tick().await;
    write(&local.join("a.txt"), "a, changed here");
    std::fs::remove_file(local.join("b.txt")).unwrap();
    write(&local.join("New/c.txt"), "c");
    engine.sync().await.unwrap();
    assert_eq!(names(&local), ["New", "a.txt", "d.txt"]);
    assert_eq!(
        read(&local.join("a.txt")).as_deref(),
        Some("a, changed here")
    );
    assert!(!server.dir.join("local-trash").exists());
    let status = engine.status().borrow().clone();
    assert_eq!((status.phase, status.error), (Phase::UpToDate, None));
    assert_eq!(
        status.waiting,
        [Waiting {
            folder: folder.id,
            why: Held::Unavailable,
            reason: reason.into(),
        }]
    );
    assert!(status.unavailable.is_empty());
    assert_eq!(
        not_synced(&engine),
        [
            held_because("Team/New", Held::Unavailable, reason),
            held_because("Team/a.txt", Held::Unavailable, reason),
            held_because("Team/b.txt", Held::Unavailable, reason),
        ]
    );

    // Back: what waited is sent, the delete to the server's trash.
    server.unlock_drive(team).await;
    engine.sync().await.unwrap();
    assert_eq!(server.names_in(root.id).await, ["New", "a.txt", "d.txt"]);
    assert_eq!(server.content(a.id).await, b"a, changed here");
    let trashed = server.remote().trashed(&team.to_string()).await.unwrap();
    assert_eq!(trashed.len(), 1);
    let status = engine.status().borrow().clone();
    assert!(status.waiting.is_empty());
    assert!(status.not_synced.is_empty());
    assert_eq!((status.phase, status.error), (Phase::UpToDate, None));
}

#[tokio::test]
async fn a_drive_being_moved_is_read_only_until_the_move_is_done() {
    let server = server().await;
    let (team, root) = server.shared_drive("Team").await;
    server.upload(root.id, "a.txt", b"a").await;
    let engine = server.bare_engine().await;
    let local = server.dir.join("local/Team");
    let folder = engine
        .add_folder(root.id, &local, Mode::Sync)
        .await
        .unwrap();
    engine.sync().await.unwrap();

    server.start_move(team).await;
    let drive = server.remote().drive(&team.to_string()).await.unwrap();
    assert_eq!(
        drive.moving_to.map(|to| to.name).as_deref(),
        Some("The server's disk")
    );
    // A write meanwhile is refused as read-only, not as a conflict.
    let temp = server.dir.join("refused.txt");
    std::fs::write(&temp, "x").unwrap();
    match server
        .remote()
        .upload_new_plain(root.id, "x.txt", &temp)
        .await
    {
        Err(Error::ReadOnly(_)) => {}
        other => panic!("not refused as read-only: {other:?}"),
    }

    // Made and deleted here meanwhile: kept, not sent, told why.
    write(&local.join("new.txt"), "new");
    std::fs::remove_file(local.join("a.txt")).unwrap();
    engine.sync().await.unwrap();
    assert_eq!(server.names_in(root.id).await, ["a.txt"]);
    assert_eq!(names(&local), ["new.txt"]);
    let reason = "Being moved to The server's disk: read-only until that is done, \
                  then what changed here is sent";
    let status = engine.status().borrow().clone();
    assert_eq!((status.phase, status.error), (Phase::UpToDate, None));
    assert_eq!(
        status.waiting,
        [Waiting {
            folder: folder.id,
            why: Held::Moving,
            reason: reason.into(),
        }]
    );
    assert_eq!(
        not_synced(&engine),
        [
            held_because("Team/a.txt", Held::Moving, reason),
            held_because("Team/new.txt", Held::Moving, reason),
        ]
    );

    // Done: sent.
    server.end_move(team).await;
    engine.sync().await.unwrap();
    assert_eq!(server.names_in(root.id).await, ["new.txt"]);
    let status = engine.status().borrow().clone();
    assert!(status.waiting.is_empty() && status.not_synced.is_empty());
}

// --- Mirrors (docs/M7D_DESIGN.md) ------------------------------------------

use organizer::files::mirrors::Schedule;
use organizer_sync::api::types::{DriveKind, MirrorStatus, Role as DriveRole};

impl Server {
    /// A mirror of `source` for the user, made on the server's side as
    /// `POST /mirrors` makes it, copied once. A mirror's drive is always on
    /// one of the user's own backends, and these tests have no S3: the
    /// backend's in-memory stand-in is one (`Backends::stand_in_own`).
    async fn mirror(&self, source: Uuid) -> (Uuid, Node) {
        let files = &self.state.files;
        let backend = files.backends().stand_in_own(self.user).await.unwrap();
        let id = files
            .create_mirror(self.user, source, backend, Schedule::Continuous)
            .await
            .unwrap();
        files.run_mirrors().await;
        let drive = self.remote().drive(&id.to_string()).await.unwrap();
        let root = self
            .remote()
            .node(drive.root_id.expect("the mirror's folder"))
            .await
            .unwrap();
        (id, root)
    }
}

#[tokio::test]
async fn a_mirror_syncs_read_only() {
    let server = server().await;
    let my = server.root().await;
    server.upload(my.id, "a.txt", b"a").await;
    server.upload(my.id, "d.txt", b"d").await;
    let (mirror, root) = server.mirror(server.user).await;

    // Listed as a mirror, after the other drives, viewed.
    let drives = server.remote().drives().await.unwrap();
    let listed = drives.last().unwrap();
    assert_eq!(listed.id, mirror.to_string());
    assert_eq!(listed.kind, DriveKind::Mirror);
    assert_eq!(listed.my_role, Some(DriveRole::Viewer));
    let of = listed.mirror_of.as_ref().expect("what it mirrors");
    let source = server.user.to_string();
    assert_eq!(of.drive_id.as_deref(), Some(source.as_str()));
    assert_eq!(of.status, MirrorStatus::UpToDate);

    let engine = server.bare_engine().await;
    let local = server.dir.join("local/Mirror");
    engine
        .add_folder(root.id, &local, Mode::Sync)
        .await
        .unwrap();
    engine.sync().await.unwrap();
    assert_eq!(names(&local), ["a.txt", "d.txt"]);

    // Nothing made, changed or deleted here reaches the mirror (nor the
    // drive it copies): kept here as it is and told; a delete is undone.
    tick().await;
    write(&local.join("new.txt"), "new");
    write(&local.join("a.txt"), "changed here");
    std::fs::remove_file(local.join("d.txt")).unwrap();
    engine.sync().await.unwrap();
    assert_eq!(server.names_in(root.id).await, ["a.txt", "d.txt"]);
    assert_eq!(server.names_in(my.id).await, ["a.txt", "d.txt"]);
    assert_eq!(read(&local.join("a.txt")).as_deref(), Some("changed here"));
    assert_eq!(read(&local.join("d.txt")).as_deref(), Some("d"));
    assert_eq!(
        not_synced(&engine),
        [
            held("Mirror/a.txt", Held::ViewOnly),
            held("Mirror/new.txt", Held::ViewOnly),
        ]
    );
    let status = engine.status().borrow().clone();
    assert_eq!((status.phase, status.error), (Phase::UpToDate, None));
    assert!(!server.dir.join("local-trash").exists());

    // What the drive gains is copied into the mirror, and comes here.
    server.upload(my.id, "c.txt", b"c").await;
    server.state.files.run_mirrors().await;
    engine.sync().await.unwrap();
    assert_eq!(read(&local.join("c.txt")).as_deref(), Some("c"));
    let status = engine.status().borrow().clone();
    assert_eq!((status.phase, status.error), (Phase::UpToDate, None));
}

#[tokio::test]
async fn an_encrypted_folder_in_a_mirror_opens_with_its_sources_keys() {
    let server = server().await;
    let me = server.set_up_encryption(&server.session).await;
    let my = server.root().await;
    let private = server.encrypted_folder(my.id, "Private").await;
    let key = crypto::random_32();
    server
        .post_key(private, server.user, 1, &key, &me.public)
        .await;
    let sub = server.sealed_folder((&key, 1), private, "Sub").await;
    server
        .sealed_file((&key, 1), private, "a.txt", b"hello")
        .await;
    server
        .sealed_file((&key, 1), sub.id, "b.txt", b"below")
        .await;
    // Copied as the server holds it: sealed, with no keys of its own.
    let (_, root) = server.mirror(server.user).await;

    let engine = server.bare_engine().await;
    let local = server.dir.join("local/Mirror");
    engine
        .add_folder(root.id, &local, Mode::Sync)
        .await
        .unwrap();
    engine.unlock(PASSPHRASE).await.unwrap();
    engine.sync().await.unwrap();
    assert_eq!(names(&local), ["Private"]);
    assert_eq!(names(&local.join("Private")), ["Sub", "a.txt"]);
    assert_eq!(read(&local.join("Private/a.txt")).as_deref(), Some("hello"));
    assert_eq!(
        read(&local.join("Private/Sub/b.txt")).as_deref(),
        Some("below")
    );
    let status = engine.status().borrow().clone();
    assert_eq!((status.phase, status.error), (Phase::UpToDate, None));
    assert_eq!(status.encryption.skipped, 0);
}

// --- Drives from other servers (docs/M7E_DESIGN.md) ------------------------

use organizer::federation::outgoing;
use organizer_sync::api::types::IncomingShareState;

impl Server {
    /// `host:port`, as OCM addresses name this server (its proxy's).
    fn host(&self) -> &str {
        self.url.trim_start_matches("http://")
    }

    /// The user's OCM address here (their email at our host).
    fn address(&self) -> String {
        format!("anna@example.com@{}", self.host())
    }

    /// Federation open here, as the superadmin sets it: through their
    /// endpoint, after a second factor confirmed (an authenticator app
    /// enabled for the user who set the server up, its codes made as an
    /// app makes them). Reachable at the proxy's address, over http on a
    /// local address (`FEDERATION_ALLOW_PRIVATE`).
    async fn federate(&self) {
        let federation = self.state.files.federation();
        federation.set_allow_private(true);
        federation.set_public_url(Some(self.url.clone()));
        let session = Some(self.session.as_str());
        let post = reqwest::Method::POST;
        let setup = self
            .json(post.clone(), "/auth/totp/setup", session, None)
            .await;
        let secret = setup["secret"].as_str().expect("a secret").to_string();
        let step = organizer::totp::current_step();
        let code = |step| organizer::totp::code_at(&secret, step).unwrap();
        self.json(
            post.clone(),
            "/auth/totp/enable",
            session,
            Some(serde_json::json!({ "code": code(step) })),
        )
        .await;
        self.json(
            post,
            "/auth/step-up/totp",
            session,
            Some(serde_json::json!({ "code": code(step + 1) })),
        )
        .await;
        self.json(
            reqwest::Method::PUT,
            "/superadmin/federation",
            session,
            Some(serde_json::json!({ "mode": "open" })),
        )
        .await;
    }

    /// Federation open here and on `other`, each the other's contact
    /// through an invite made there.
    async fn federated_with(&self, other: &Server) {
        self.federate().await;
        other.federate().await;
        let invite = other
            .json(
                reqwest::Method::POST,
                "/invites/federated",
                Some(&other.session),
                Some(serde_json::json!({})),
            )
            .await;
        self.json(
            reqwest::Method::POST,
            "/invites/federated/accept",
            Some(&self.session),
            Some(serde_json::json!({ "invite": invite["invite"] })),
        )
        .await;
    }
}

/// A shared drive on another Organizer server, given to the user here as a
/// viewer (`kind` `remote`): once accepted it is synced like any drive, by
/// the viewer's rules (nothing made, changed or deleted here is sent, a
/// delete is undone), its bytes through the user's own server, and what
/// changes there comes here.
#[tokio::test(flavor = "multi_thread")]
async fn a_drive_from_another_server_syncs_by_its_role() {
    let (home, other) = (server().await, server().await);
    home.federated_with(&other).await;
    // Live notices from the other server, so its changes come at once.
    tokio::spawn(outgoing::run_live(other.state.files.clone()));

    // A drive there, with files, shared with the user here as a viewer.
    let files = &other.state.files;
    let team = files
        .create_shared_drive("Team", None, other.user)
        .await
        .unwrap();
    files.set_member(team, other.user, true).await.unwrap();
    let team_root = files.root_node(team).await.unwrap().id;
    other.upload(team_root, "a.txt", b"a").await;
    other.upload(team_root, "d.txt", b"d").await;
    other
        .json(
            reqwest::Method::POST,
            &format!("/drives/{team}/access"),
            Some(&other.session),
            Some(serde_json::json!({ "address": home.address(), "role": "viewer" })),
        )
        .await;

    // Listed here as a drive from another server, viewed, waiting to be
    // accepted.
    let drives = home.remote().drives().await.unwrap();
    let listed = drives
        .iter()
        .find(|d| d.kind == DriveKind::Remote)
        .expect("the drive from the other server");
    assert_eq!(listed.name, "Team");
    assert_eq!(listed.my_role, Some(DriveRole::Viewer));
    let from = listed.remote.as_ref().expect("where it comes from");
    assert_eq!(from.server, other.host());
    assert_eq!(from.state, IncomingShareState::Pending);
    let root = listed.root_id.expect("the drive's folder");
    home.json(
        reqwest::Method::POST,
        &format!("/shared/{root}/accept"),
        None,
        None,
    )
    .await;

    let engine = home.bare_engine().await;
    let local = home.dir.join("local/Team");
    engine.add_folder(root, &local, Mode::Sync).await.unwrap();
    engine.sync().await.unwrap();
    assert_eq!(names(&local), ["a.txt", "d.txt"]);
    assert_eq!(read(&local.join("a.txt")).as_deref(), Some("a"));

    // Nothing made, changed or deleted here reaches the other server: kept
    // here as it is and told; a delete is undone.
    tick().await;
    write(&local.join("new.txt"), "new");
    write(&local.join("a.txt"), "changed here");
    std::fs::remove_file(local.join("d.txt")).unwrap();
    engine.sync().await.unwrap();
    assert_eq!(other.names_in(team_root).await, ["a.txt", "d.txt"]);
    assert_eq!(read(&local.join("a.txt")).as_deref(), Some("changed here"));
    assert_eq!(read(&local.join("d.txt")).as_deref(), Some("d"));
    assert_eq!(
        not_synced(&engine),
        [
            held("Team/a.txt", Held::ViewOnly),
            held("Team/new.txt", Held::ViewOnly),
        ]
    );
    let status = engine.status().borrow().clone();
    assert_eq!((status.phase, status.error), (Phase::UpToDate, None));
    assert!(!home.dir.join("local-trash").exists());

    // What is added there comes here.
    other.upload(team_root, "c.txt", b"c").await;
    for _ in 0..50 {
        engine.sync().await.unwrap();
        if local.join("c.txt").exists() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    assert_eq!(read(&local.join("c.txt")).as_deref(), Some("c"));
    let status = engine.status().borrow().clone();
    assert_eq!((status.phase, status.error), (Phase::UpToDate, None));
}

/// An encrypted folder shared from another Organizer server
/// (`organizer-e2e`, docs/E2E_SPEC.md §12), given to the user here as an
/// editor. The owner's device there wraps its key for them (a member marked
/// `remote`, by the share's id); their device here opens it with the
/// owner's ids (`source_id`: the keys wrapped for the owner's folder, the
/// names sealed with the owner's parents) and writes in it, sealing new
/// names, renames and moves with the parents' sources, which a folder made
/// here gets once the owner's server has it.
#[tokio::test(flavor = "multi_thread")]
async fn an_encrypted_folder_from_another_server_opens_and_is_written_with_its_ids() {
    let (home, other) = (server().await, server().await);
    home.federated_with(&other).await;
    home.set_up_encryption(&home.session).await;
    let owner = other.set_up_encryption(&other.session).await;

    // On the other server: an encrypted folder with a key, sealed in it a
    // folder and two files.
    let top = other.root().await;
    let private = other.encrypted_folder(top.id, "Private").await;
    let key = crypto::random_32();
    other
        .post_key(private, other.user, 1, &key, &owner.public)
        .await;
    let sub = other.sealed_folder((&key, 1), private, "Sub").await;
    other
        .sealed_file((&key, 1), private, "a.txt", b"hello")
        .await;
    other
        .sealed_file((&key, 1), sub.id, "b.txt", b"below")
        .await;

    // Shared with the user here: their public key comes from this server.
    let grant = other
        .json(
            reqwest::Method::POST,
            &format!("/files/{private}/access"),
            Some(&other.session),
            Some(serde_json::json!({ "address": home.address(), "role": "editor" })),
        )
        .await;
    let share = grant["id"].as_str().expect("the share's id").to_string();
    // The owner's device, unlocked, wraps the key for them by the share's id.
    let owners = other.bare_engine().await;
    owners.unlock(PASSPHRASE).await.unwrap();
    owners.sync().await.unwrap();
    let keys = other
        .json(
            reqwest::Method::GET,
            &format!("/encryption/folders/{private}"),
            None,
            None,
        )
        .await;
    let member = keys["members"]
        .as_array()
        .unwrap()
        .iter()
        .find(|m| m["remote"] == home.address())
        .cloned()
        .expect("the member on the other server");
    assert_eq!(member["user_id"], share);
    assert_eq!(member["missing"], serde_json::json!([]));

    // Accepted here; the owner's ids read into the index.
    let shared = home.json(reqwest::Method::GET, "/shared", None, None).await;
    let item = shared
        .as_array()
        .unwrap()
        .iter()
        .find(|s| s["remote"]["state"] == "pending")
        .cloned()
        .expect("a pending share from the other server");
    let root: Uuid = item["node"]["id"].as_str().unwrap().parse().unwrap();
    home.json(
        reqwest::Method::POST,
        &format!("/shared/{root}/accept"),
        None,
        None,
    )
    .await;
    let follow = || async {
        let ids: Vec<Uuid> =
            sqlx::query_scalar("SELECT id FROM ocm_received WHERE state = 'accepted'")
                .fetch_all(home.state.db.reader())
                .await
                .unwrap();
        for id in ids {
            organizer::federation::incoming::follow(&home.state.files, id)
                .await
                .unwrap();
        }
    };
    follow().await;
    assert_eq!(
        home.remote().node(root).await.unwrap().source_id,
        Some(private)
    );

    // Opened here, decrypted.
    let engine = home.bare_engine().await;
    let local = home.dir.join("local/Private");
    engine.add_folder(root, &local, Mode::Sync).await.unwrap();
    engine.unlock(PASSPHRASE).await.unwrap();
    engine.sync().await.unwrap();
    assert_eq!(names(&local), ["Sub", "a.txt"]);
    assert_eq!(read(&local.join("a.txt")).as_deref(), Some("hello"));
    assert_eq!(read(&local.join("Sub/b.txt")).as_deref(), Some("below"));
    let status = engine.status().borrow().clone();
    assert_eq!((status.phase, status.error), (Phase::UpToDate, None));
    assert_eq!(status.encryption.skipped, 0);

    // Written here: a new file, a new folder with a file, a rename and a
    // move; on the owner's server, sealed with the owner's ids.
    write(&local.join("new.txt"), "secret");
    write(&local.join("Made/inner.txt"), "inside");
    std::fs::rename(local.join("a.txt"), local.join("renamed.txt")).unwrap();
    std::fs::rename(local.join("Sub/b.txt"), local.join("moved.txt")).unwrap();
    for _ in 0..3 {
        engine.sync().await.unwrap();
        follow().await;
    }
    let open = |bytes: &[u8]| contents::decrypt(|_| Some(key), bytes).unwrap();
    let children = other.sealed_children(private, &[key]).await;
    assert_eq!(
        names_of(&children),
        ["Made", "Sub", "moved.txt", "new.txt", "renamed.txt"]
    );
    let new = node_named(&children, "new.txt");
    let bytes = other.content(new.id).await;
    assert!(!bytes.windows(6).any(|w| w == b"secret"));
    assert_eq!(open(&bytes), b"secret");
    assert_eq!(
        open(&other.content(node_named(&children, "moved.txt").id).await),
        b"below"
    );
    let made = node_named(&children, "Made").id;
    let inner = other.sealed_children(made, &[key]).await;
    assert_eq!(names_of(&inner), ["inner.txt"]);
    assert_eq!(open(&other.content(inner[0].1.id).await), b"inside");
    assert!(other.sealed_children(sub.id, &[key]).await.is_empty());

    // Here everything agreed on, and the member's device has no tasks.
    engine.sync().await.unwrap();
    assert_eq!(
        names(&local),
        ["Made", "Sub", "moved.txt", "new.txt", "renamed.txt"]
    );
    let status = engine.status().borrow().clone();
    assert_eq!((status.phase, status.error), (Phase::UpToDate, None));
    let pending = home
        .json(reqwest::Method::GET, "/encryption/pending", None, None)
        .await;
    assert_eq!(pending, serde_json::json!([]));
}

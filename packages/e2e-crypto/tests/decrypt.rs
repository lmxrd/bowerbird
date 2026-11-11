//! organizer-decrypt against a real server, in-process, with folders made
//! and filled the way the apps do (keys wrapped, names and contents
//! sealed): from the server through the API, and from its data folder as a
//! restored backup would have it.

use organizer::state::AppState;
use organizer_e2e_crypto::tool::{self, Args, Secret, Which};
use organizer_e2e_crypto::{b64, contents, folder, keys, names};
use serde_json::{Value, json};
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use uuid::Uuid;

const PASSPHRASE: &str = "a passphrase of my own";
const EMAIL: &str = "anna@example.com";

struct Server {
    url: String,
    data: PathBuf,
    dir: PathBuf,
    http: reqwest::Client,
    session: String,
    key: String,
}

impl Server {
    async fn send(&self, request: reqwest::RequestBuilder) -> Value {
        let response = request
            .header("cookie", &self.session)
            .send()
            .await
            .unwrap();
        let status = response.status();
        let body: Value = response.json().await.unwrap_or_default();
        assert!(status.is_success(), "{status}: {body}");
        body
    }

    async fn post(&self, path: &str, body: Value) -> Value {
        self.send(
            self.http
                .post(format!("{}/api{path}", self.url))
                .json(&body),
        )
        .await
    }

    async fn get(&self, path: &str) -> Value {
        self.send(self.http.get(format!("{}/api{path}", self.url)))
            .await
    }

    /// Upload a file as the apps do: a multipart form.
    async fn upload(&self, folder: Uuid, name: &str, bytes: Vec<u8>) -> Value {
        let mut body = format!(
            "--b\r\nContent-Disposition: form-data; name=\"file\"; filename=\"{name}\"\r\nContent-Type: application/octet-stream\r\n\r\n"
        )
        .into_bytes();
        body.extend(bytes);
        body.extend(b"\r\n--b--\r\n");
        self.send(
            self.http
                .post(format!("{}/api/files/{folder}/files", self.url))
                .header("content-type", "multipart/form-data; boundary=b")
                .body(body),
        )
        .await
    }
}

async fn server() -> Server {
    organizer::install_tls_provider();
    let dir = std::env::temp_dir().join(format!("organizer-decrypt-{}", Uuid::new_v4().simple()));
    let data = dir.join("data");
    std::fs::create_dir_all(&data).unwrap();
    let db = organizer::db::open(data.join("organizer.db").to_str().unwrap())
        .await
        .unwrap();
    let state = AppState {
        oidc: Arc::new(organizer::oidc::Oidc::load(&db, organizer::oidc::Env::default()).await),
        files: organizer::files::Files::new(db.clone(), data.clone(), None),
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
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    let app = organizer::app(state);
    tokio::spawn(async move {
        axum::serve(
            listener,
            app.into_make_service_with_connect_info::<SocketAddr>(),
        )
        .await
        .unwrap();
    });
    let http = reqwest::Client::new();
    let setup = http
        .post(format!("{url}/api/auth/setup"))
        .json(&json!({ "email": EMAIL, "password": "a long enough password" }))
        .send()
        .await
        .unwrap();
    assert!(setup.status().is_success());
    let session = setup
        .headers()
        .get("set-cookie")
        .unwrap()
        .to_str()
        .unwrap()
        .split(';')
        .next()
        .unwrap()
        .to_string();
    let mut server = Server {
        url,
        data,
        dir,
        http,
        session,
        key: String::new(),
    };
    let key = server
        .post(
            "/auth/api-keys",
            json!({ "name": "decrypt", "scope": "read" }),
        )
        .await;
    server.key = key["token"].as_str().unwrap().to_string();
    server
}

fn pattern(len: usize, seed: u8) -> Vec<u8> {
    (0..len)
        .map(|i| (i as u8).wrapping_mul(31) ^ seed)
        .collect()
}

/// Encryption set up and an encrypted folder filled as a device would, its
/// key turned over once (so both generations are in it): answers the
/// recovery key, and what each path in the folder should hold.
async fn fill(server: &Server) -> (String, Vec<(String, Vec<u8>)>) {
    let identity = keys::Identity::generate();
    let salt = organizer_e2e_crypto::random::<16>();
    let recovery = organizer_e2e_crypto::random_32();
    let passphrase_wrapped =
        keys::wrap_with_passphrase(&identity.private, PASSPHRASE, &salt, keys::MIN_ITERATIONS);
    let recovery_wrapped = keys::wrap_private(
        &keys::recovery_wrapping_key(&recovery),
        &identity.private,
        &organizer_e2e_crypto::random(),
    );
    server
        .post(
            "/encryption",
            json!({
                "public_key": b64(&identity.public),
                "passphrase": {
                    "salt": b64(&salt),
                    "iterations": keys::MIN_ITERATIONS,
                    "wrapped": b64(&passphrase_wrapped),
                },
                "recovery_wrapped": b64(&recovery_wrapped),
            }),
        )
        .await;
    let me = server.get("/auth/session").await["user"]["id"]
        .as_str()
        .unwrap()
        .to_string();
    let root: Uuid = server.get("/files/root").await["id"]
        .as_str()
        .unwrap()
        .parse()
        .unwrap();
    let private: Uuid = server
        .post(
            &format!("/files/{root}/folders"),
            json!({ "name": "Private", "encrypted": true }),
        )
        .await["id"]
        .as_str()
        .unwrap()
        .parse()
        .unwrap();
    let key1 = organizer_e2e_crypto::random_32();
    let wrapped = folder::wrap(&key1, private, 1, &identity.public).unwrap();
    server
        .post(
            &format!("/encryption/folders/{private}/keys"),
            json!({ "keys": [{ "user_id": me, "generation": 1, "wrapped": b64(&wrapped) }] }),
        )
        .await;

    let mut expected = Vec::new();
    let big = pattern(3 * contents::SEGMENT + 123, 7);
    let name = names::encrypt(&key1, 1, private, "Taxes 2026.pdf").unwrap();
    server
        .upload(private, &name, contents::encrypt(&key1, 1, &big))
        .await;
    expected.push(("Taxes 2026.pdf".to_string(), big));
    let sub_name = names::encrypt(&key1, 1, private, "Récépissés").unwrap();
    let sub: Uuid = server
        .post(
            &format!("/files/{private}/folders"),
            json!({ "name": sub_name }),
        )
        .await["id"]
        .as_str()
        .unwrap()
        .parse()
        .unwrap();
    let name = names::encrypt(&key1, 1, sub, "empty.txt").unwrap();
    server
        .upload(sub, &name, contents::encrypt(&key1, 1, b""))
        .await;
    expected.push(("Récépissés/empty.txt".to_string(), Vec::new()));

    // A new key, as after someone left: written after it uses it.
    let key2 = organizer_e2e_crypto::random_32();
    let wrapped = folder::wrap(&key2, private, 2, &identity.public).unwrap();
    server
        .post(
            &format!("/encryption/folders/{private}/keys"),
            json!({ "keys": [{ "user_id": me, "generation": 2, "wrapped": b64(&wrapped) }] }),
        )
        .await;
    let note = b"written with the second key".to_vec();
    let name = names::encrypt(&key2, 2, sub, "note ✓.txt").unwrap();
    server
        .upload(sub, &name, contents::encrypt(&key2, 2, &note))
        .await;
    expected.push(("Récépissés/note ✓.txt".to_string(), note));
    (keys::encode_recovery(&recovery), expected)
}

fn check(out: &Path, expected: &[(String, Vec<u8>)]) {
    for (path, bytes) in expected {
        let found = std::fs::read(out.join(path)).unwrap_or_else(|e| panic!("{path}: {e}"));
        assert_eq!(&found, bytes, "{path}");
    }
    let mut count = 0;
    let mut todo = vec![out.to_path_buf()];
    while let Some(dir) = todo.pop() {
        for entry in std::fs::read_dir(dir).unwrap() {
            let entry = entry.unwrap();
            if entry.file_type().unwrap().is_dir() {
                todo.push(entry.path());
            } else {
                count += 1;
            }
        }
    }
    assert_eq!(count, expected.len(), "nothing else, no leftovers");
}

fn args(list: &[&str]) -> Args {
    Args::parse(list.iter().map(|s| s.to_string())).unwrap_or_else(|e| panic!("{e}"))
}

#[tokio::test]
async fn a_folder_is_decrypted_from_the_server_and_from_its_data_folder() {
    let server = server().await;
    let (recovery, expected) = fill(&server).await;
    let passphrase = Secret::Passphrase(PASSPHRASE.into());

    // From the server, with the passphrase.
    let out = server.dir.join("from-server");
    let report = tool::run(
        &args(&[
            "--server",
            &server.url,
            "--api-key",
            &server.key,
            "--folder",
            "/Private",
            "--out",
            out.to_str().unwrap(),
        ]),
        &passphrase,
    )
    .await
    .unwrap();
    assert_eq!((report.files, report.folders), (3, 1));
    assert!(report.failed.is_empty(), "{:?}", report.failed);
    check(&out, &expected);
    // Nothing is overwritten: a second run leaves what is there.
    let again = tool::run(
        &args(&[
            "--server",
            &server.url,
            "--api-key",
            &server.key,
            "--folder",
            "/Private",
            "--out",
            out.to_str().unwrap(),
        ]),
        &passphrase,
    )
    .await
    .unwrap();
    assert_eq!(again.files, 0);
    assert_eq!(again.failed.len(), 3);
    check(&out, &expected);

    // From the data folder, with the recovery key.
    let out = server.dir.join("from-backup");
    let report = tool::run(
        &args(&[
            "--data-dir",
            server.data.to_str().unwrap(),
            "--user",
            EMAIL,
            "--folder",
            "/Private",
            "--out",
            out.to_str().unwrap(),
            "--recovery-key",
        ]),
        &Secret::RecoveryKey(recovery.to_lowercase()),
    )
    .await
    .unwrap();
    assert!(report.failed.is_empty(), "{:?}", report.failed);
    check(&out, &expected);

    // A wrong passphrase, a plain folder, a folder in it: refused.
    let backup = |folder: &str, out: &str| {
        args(&[
            "--data-dir",
            server.data.to_str().unwrap(),
            "--user",
            EMAIL,
            "--folder",
            folder,
            "--out",
            out,
        ])
    };
    let wrong = tool::run(
        &backup("/Private", "x"),
        &Secret::Passphrase("not it".into()),
    )
    .await;
    assert_eq!(wrong.unwrap_err().to_string(), "Wrong passphrase");
    let plain = tool::run(&backup("/", "x"), &passphrase).await;
    assert_eq!(
        plain.unwrap_err().to_string(),
        "That folder is not encrypted"
    );
    let missing = tool::run(&backup("/Elsewhere", "x"), &passphrase).await;
    assert!(missing.is_err());
    assert!(matches!(
        Args::parse(["--folder", "/x", "--out", "y"].map(String::from)),
        Err(tool::ToolError::Usage(_))
    ));
    assert!(matches!(&backup("/Private", "x").which, Which::Path(p) if p == "/Private"));

    let _ = std::fs::remove_dir_all(&server.dir);
}

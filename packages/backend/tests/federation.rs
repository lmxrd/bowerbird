//! Federation (docs/M7E_DESIGN.md): two Organizer servers in this process,
//! each on its own local socket, talking OCM to each other over real HTTP
//! (`FEDERATION_ALLOW_PRIVATE`: http and local addresses). The tests drive
//! them through their APIs as their users would, and reach into their
//! state only for what no API shows (a share's secret, a follow round).

use organizer::federation::{incoming, outgoing};
use organizer::{app, db, state::AppState};
use serde_json::{Value, json};
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

/// A server: where it listens and its state.
struct Server {
    base: String,
    /// `host:port`, as OCM addresses name it.
    host: String,
    state: AppState,
    http: reqwest::Client,
}

async fn server() -> Server {
    organizer::install_tls_provider();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let base = format!("http://{addr}");
    let db = db::open(":memory:").await.unwrap();
    let data_dir =
        std::env::temp_dir().join(format!("organizer-federation-{}", uuid::Uuid::now_v7()));
    std::fs::create_dir_all(&data_dir).unwrap();
    let mut state = AppState::new(db, data_dir).await;
    state.public_url = Some(base.clone());
    state.limits = Arc::new(organizer::limits::Limits::with_bursts(1_000, 1_000, 1_000));
    let federation = state.files.federation();
    federation.set_allow_private(true);
    federation.set_public_url(Some(base.clone()));
    let service = app(state.clone()).into_make_service_with_connect_info::<SocketAddr>();
    tokio::spawn(async move { axum::serve(listener, service).await.unwrap() });
    tokio::spawn(outgoing::run_live(state.files.clone()));
    Server {
        host: addr.to_string(),
        base,
        state,
        http: reqwest::Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .unwrap(),
    }
}

/// A user's session on a server.
#[derive(Clone)]
struct Session {
    cookie: String,
    email: String,
}

impl Server {
    async fn call(
        &self,
        method: reqwest::Method,
        path: &str,
        session: Option<&Session>,
        body: Option<Value>,
    ) -> (u16, Value) {
        let mut request = self.http.request(method, format!("{}{path}", self.base));
        if let Some(session) = session {
            request = request.header("cookie", &session.cookie);
        }
        if let Some(body) = body {
            request = request.json(&body);
        }
        let response = request.send().await.unwrap();
        let status = response.status().as_u16();
        let text = response.text().await.unwrap();
        (
            status,
            serde_json::from_str(&text).unwrap_or(Value::String(text)),
        )
    }

    async fn get(&self, path: &str, session: &Session) -> (u16, Value) {
        self.call(reqwest::Method::GET, path, Some(session), None)
            .await
    }

    async fn post(&self, path: &str, session: &Session, body: Value) -> (u16, Value) {
        self.call(reqwest::Method::POST, path, Some(session), Some(body))
            .await
    }

    /// The first superadmin, stepped up so the superadmin's endpoints
    /// answer.
    async fn admin(&self) -> Session {
        let email = "admin@a.test".to_string();
        let response = self
            .http
            .post(format!("{}/api/auth/setup", self.base))
            .json(&json!({ "email": email, "password": "correct horse battery" }))
            .send()
            .await
            .unwrap();
        assert_eq!(response.status().as_u16(), 201);
        let session = Session {
            cookie: cookie(&response),
            email,
        };
        self.step_up(&session).await;
        session
    }

    async fn step_up(&self, session: &Session) {
        let (_, me) = self.get("/api/auth/session", session).await;
        let user = me["user"]["id"].as_str().unwrap().to_string();
        let (_, id) = session.cookie.split_once('=').unwrap();
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs() as i64;
        sqlx::query(
            "UPDATE sessions SET data = json_set(data, '$.\"organizer.step_up\"', json(?1))
             WHERE id_hash = ?2",
        )
        .bind(json!({ "user": user, "at": now }).to_string())
        .bind(organizer::util::sha256_hex(id))
        .execute(self.state.db.writer())
        .await
        .unwrap();
    }

    /// A user made through an invite of the superadmin, signed in.
    async fn user(&self, admin: &Session, email: &str) -> Session {
        let (status, invite) = self
            .post("/api/superadmin/invites", admin, json!({ "email": email }))
            .await;
        assert_eq!(status, 201, "{invite}");
        let response = self
            .http
            .post(format!(
                "{}/api/auth/invite/{}",
                self.base,
                invite["token"].as_str().unwrap()
            ))
            .json(&json!({ "email": email, "password": "correct horse battery" }))
            .send()
            .await
            .unwrap();
        assert_eq!(response.status().as_u16(), 201);
        Session {
            cookie: cookie(&response),
            email: email.to_string(),
        }
    }

    async fn federation(&self, admin: &Session, body: Value) {
        let (status, answer) = self
            .call(
                reqwest::Method::PUT,
                "/api/superadmin/federation",
                Some(admin),
                Some(body),
            )
            .await;
        assert_eq!(status, 200, "{answer}");
    }

    /// A superadmin's setting, as a default.
    async fn setting(&self, admin: &Session, key: &str, value: Value) {
        let (status, answer) = self
            .call(
                reqwest::Method::PUT,
                &format!("/api/superadmin/settings/{key}"),
                Some(admin),
                Some(json!({ "mode": "default", "value": value })),
            )
            .await;
        assert_eq!(status, 200, "{answer}");
    }

    /// A user's OCM address here.
    fn address(&self, session: &Session) -> String {
        format!("{}@{}", session.email, self.host)
    }

    async fn root(&self, session: &Session) -> String {
        let (_, root) = self.get("/api/files/root", session).await;
        root["id"].as_str().unwrap().to_string()
    }

    async fn folder(&self, session: &Session, parent: &str, name: &str) -> String {
        let (status, made) = self
            .post(
                &format!("/api/files/{parent}/folders"),
                session,
                json!({ "name": name }),
            )
            .await;
        assert_eq!(status, 201, "{made}");
        made["id"].as_str().unwrap().to_string()
    }

    async fn upload(
        &self,
        session: &Session,
        parent: &str,
        name: &str,
        content: &[u8],
    ) -> (u16, Value) {
        let form = reqwest::multipart::Form::new().part(
            "file",
            reqwest::multipart::Part::bytes(content.to_vec()).file_name(name.to_string()),
        );
        let response = self
            .http
            .post(format!("{}/api/files/{parent}/files", self.base))
            .header("cookie", &session.cookie)
            .multipart(form)
            .send()
            .await
            .unwrap();
        let status = response.status().as_u16();
        (status, response.json().await.unwrap_or_default())
    }

    async fn download(&self, session: &Session, id: &str) -> (u16, Vec<u8>) {
        let response = self
            .http
            .get(format!("{}/api/files/{id}/content", self.base))
            .header("cookie", &session.cookie)
            .send()
            .await
            .unwrap();
        let status = response.status().as_u16();
        (status, response.bytes().await.unwrap().to_vec())
    }

    async fn children(&self, session: &Session, id: &str) -> Vec<Value> {
        let (status, listed) = self
            .get(&format!("/api/files/{id}/children"), session)
            .await;
        assert_eq!(status, 200, "{listed}");
        listed["items"].as_array().unwrap().clone()
    }

    async fn shared_with_me(&self, session: &Session) -> Vec<Value> {
        let (status, listed) = self.get("/api/shared", session).await;
        assert_eq!(status, 200, "{listed}");
        listed.as_array().unwrap().clone()
    }

    /// Follow every accepted share from another server once.
    async fn follow(&self) {
        let ids: Vec<uuid::Uuid> =
            sqlx::query_scalar("SELECT id FROM ocm_received WHERE state = 'accepted'")
                .fetch_all(self.state.db.reader())
                .await
                .unwrap();
        for id in ids {
            incoming::follow(&self.state.files, id).await.unwrap();
        }
    }
}

fn cookie(response: &reqwest::Response) -> String {
    response
        .headers()
        .get_all("set-cookie")
        .iter()
        .filter_map(|v| v.to_str().ok())
        .find(|v| v.starts_with("organizer_session"))
        .expect("a session cookie")
        .split(';')
        .next()
        .unwrap()
        .to_string()
}

fn named<'a>(items: &'a [Value], name: &str) -> &'a Value {
    items
        .iter()
        .find(|i| i["name"] == name)
        .unwrap_or_else(|| panic!("no {name} in {items:?}"))
}

/// Two servers with federation open, a superadmin each, alice on A and
/// bob on B, and each the other's contact through an invite.
struct Pair {
    a: Server,
    b: Server,
    a_admin: Session,
    b_admin: Session,
    alice: Session,
    bob: Session,
}

async fn pair() -> Pair {
    let (a, b) = (server().await, server().await);
    let (a_admin, b_admin) = (a.admin().await, b.admin().await);
    a.federation(&a_admin, json!({ "mode": "open" })).await;
    b.federation(&b_admin, json!({ "mode": "open" })).await;
    let alice = a.user(&a_admin, "alice@a.test").await;
    let bob = b.user(&b_admin, "bob@b.test").await;
    let (status, invite) = a.post("/api/invites/federated", &alice, json!({})).await;
    assert_eq!(status, 201, "{invite}");
    let (status, contact) = b
        .post(
            "/api/invites/federated/accept",
            &bob,
            json!({ "invite": invite["invite"] }),
        )
        .await;
    assert_eq!(status, 201, "{contact}");
    Pair {
        a,
        b,
        a_admin,
        b_admin,
        alice,
        bob,
    }
}

impl Pair {
    /// Alice shares an item with bob with a role; bob accepts it. The
    /// grant on A and the shared item on B.
    async fn share(&self, id: &str, role: &str) -> (Value, Value) {
        let (status, grant) = self
            .a
            .post(
                &format!("/api/files/{id}/access"),
                &self.alice,
                json!({ "address": self.b.address(&self.bob), "role": role }),
            )
            .await;
        assert_eq!(status, 201, "{grant}");
        let shared = self.b.shared_with_me(&self.bob).await;
        let item = shared
            .iter()
            .find(|s| {
                s["remote"]["owner"] == self.a.address(&self.alice)
                    && s["remote"]["state"] == "pending"
            })
            .cloned()
            .expect("a pending share from A");
        let (status, answer) = self
            .b
            .post(
                &format!(
                    "/api/shared/{}/accept",
                    item["node"]["id"].as_str().unwrap()
                ),
                &self.bob,
                json!({}),
            )
            .await;
        assert_eq!(status, 204, "{answer}");
        (grant, item)
    }
}

/// Discovery, WebFinger and the JWK Set answer only while federation is
/// on, with the fields, capabilities and criteria of §2.
#[tokio::test]
async fn discovery_webfinger_and_the_key() {
    let a = server().await;
    let admin = a.admin().await;
    let (status, off) = a
        .call(reqwest::Method::GET, "/.well-known/ocm", None, None)
        .await;
    assert_eq!(status, 200);
    assert_eq!(off["enabled"], false);
    let (status, _) = a.call(reqwest::Method::GET, "/ocm/jwks", None, None).await;
    assert_eq!(status, 404);
    let webfinger = format!(
        "/.well-known/webfinger?resource=acct:admin%40a.test%40{}",
        a.host
    );
    let (status, _) = a.call(reqwest::Method::GET, &webfinger, None, None).await;
    assert_eq!(status, 404, "off");

    a.federation(
        &admin,
        json!({ "mode": "open", "refused": ["evil.example"] }),
    )
    .await;
    let (_, on) = a
        .call(reqwest::Method::GET, "/.well-known/ocm", None, None)
        .await;
    assert_eq!(on["enabled"], true);
    assert_eq!(on["endPoint"], format!("{}/ocm", a.base));
    assert_eq!(on["jwksUri"], format!("{}/ocm/jwks", a.base));
    assert_eq!(on["tokenEndPoint"], format!("{}/ocm/token", a.base));
    for capability in [
        "http-sig",
        "invites",
        "notifications",
        "exchange-token",
        "organizer-drives",
        "organizer-e2e",
        "organizer-live",
    ] {
        assert!(
            on["capabilities"]
                .as_array()
                .unwrap()
                .iter()
                .any(|c| c == capability),
            "{capability}"
        );
    }
    let criteria = on["criteria"].as_array().unwrap();
    for criterion in ["must-use-http-sig", "must-exchange-token", "denylist"] {
        assert!(criteria.iter().any(|c| c == criterion), "{criterion}");
    }
    let types: Vec<&str> = on["resourceTypes"]
        .as_array()
        .unwrap()
        .iter()
        .map(|t| t["name"].as_str().unwrap())
        .collect();
    assert_eq!(types, ["file", "folder"]);
    assert_eq!(on["resourceTypes"][1]["shareTypes"], json!(["user"]));
    assert_eq!(on["resourceTypes"][1]["protocols"]["webdav"], "/ocm/dav/");

    let (status, keys) = a.call(reqwest::Method::GET, "/ocm/jwks", None, None).await;
    assert_eq!(status, 200);
    let key = &keys["keys"][0];
    assert_eq!(key["kty"], "OKP");
    assert_eq!(key["crv"], "Ed25519");
    assert_eq!(key["alg"], "Ed25519");
    assert!(
        key["kid"]
            .as_str()
            .unwrap()
            .starts_with(&format!("{}#", a.host))
    );

    let (status, jrd) = a.call(reqwest::Method::GET, &webfinger, None, None).await;
    assert_eq!(status, 200, "{jrd}");
    assert_eq!(jrd["subject"], format!("acct:admin@a.test@{}", a.host));
    assert_eq!(
        jrd["links"][0]["href"],
        format!("{}/.well-known/ocm", a.base)
    );
    let nobody = format!(
        "/.well-known/webfinger?resource=acct:nobody%40a.test%40{}",
        a.host
    );
    let (status, _) = a.call(reqwest::Method::GET, &nobody, None, None).await;
    assert_eq!(status, 404);

    // Listed: an allowlist.
    a.federation(
        &admin,
        json!({ "mode": "listed", "allowed": ["b.example:8443"] }),
    )
    .await;
    let (_, listed) = a
        .call(reqwest::Method::GET, "/.well-known/ocm", None, None)
        .await;
    assert!(
        listed["criteria"]
            .as_array()
            .unwrap()
            .iter()
            .any(|c| c == "allowlist")
    );
    let (_, settings) = a.get("/api/superadmin/federation", &admin).await;
    assert_eq!(settings["mode"], "listed");
    assert_eq!(settings["allowed"][0]["host"], "b.example:8443");
    assert_eq!(settings["host"], a.host);
}

/// Every request between servers is signed: an unsigned one, one signed
/// with a key not in the signer's JWK Set, and a stale one are refused;
/// a good one gets past the signature.
#[tokio::test]
async fn requests_between_servers_are_signed() {
    let Pair { a, b, alice, .. } = pair().await;
    let body = json!({
        "shareWith": a.address(&alice),
        "name": "x",
        "providerId": "1",
        "owner": format!("someone@{}", b.host),
        "sender": format!("someone@{}", b.host),
        "shareType": "user",
        "resourceType": "file",
        "protocol": { "name": "multi", "webdav": { "uri": "x", "sharedSecret": "s", "requirements": ["must-exchange-token"] } },
    })
    .to_string();
    let url = format!("{}/ocm/shares", a.base);
    // Unsigned.
    let response = reqwest::Client::new()
        .post(&url)
        .header("content-type", "application/json")
        .body(body.clone())
        .send()
        .await
        .unwrap();
    assert_eq!(response.status().as_u16(), 401);
    // Signed with another key under B's key id.
    let kid = b.state.files.federation().kid().unwrap();
    let send = |headers: Vec<(&'static str, String)>| {
        let mut request = reqwest::Client::new()
            .post(&url)
            .header("content-type", "application/json");
        for (name, value) in headers {
            request = request.header(name, value);
        }
        request.body(body.clone()).send()
    };
    let wrong =
        organizer::federation::sign::sign(&[9u8; 32], &kid, "POST", &url, body.as_bytes()).unwrap();
    assert_eq!(send(wrong).await.unwrap().status().as_u16(), 401);
    // B's own key, but made ten minutes ago.
    let seed = b.state.files.federation().key().unwrap().seed;
    let old = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs()
        - 600;
    let stale =
        organizer::federation::sign::sign_at(&seed, &kid, "POST", &url, body.as_bytes(), old)
            .unwrap();
    assert_eq!(send(stale).await.unwrap().status().as_u16(), 401);
    // Good: past the signature (B is no contact of alice: refused for that).
    let good =
        organizer::federation::sign::sign(&seed, &kid, "POST", &url, body.as_bytes()).unwrap();
    assert_eq!(send(good).await.unwrap().status().as_u16(), 403);
    // The token endpoint takes only signed requests too.
    let response = reqwest::Client::new()
        .post(format!("{}/ocm/token", a.base))
        .header("content-type", "application/x-www-form-urlencoded")
        .body(format!(
            "grant_type=authorization_code&client_id={}&code=x",
            b.host
        ))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status().as_u16(), 400);
    let answer: Value = response.json().await.unwrap();
    assert_eq!(answer["error"], "invalid_client");
}

/// An invite accepted makes both contacts; an invite is used once.
#[tokio::test]
async fn invites_make_contacts_both_ways() {
    let Pair {
        a,
        b,
        alice,
        bob,
        b_admin,
        ..
    } = pair().await;
    let (_, mine) = a.get("/api/contacts", &alice).await;
    assert_eq!(mine[0]["address"], b.address(&bob));
    assert_eq!(mine[0]["server"], b.host);
    // What their server is, from its discovery, on both sides.
    assert_eq!(mine[0]["provider"], "Organizer");
    assert_eq!(mine[0]["organizer"], true);
    let (_, theirs) = b.get("/api/contacts", &bob).await;
    assert_eq!(theirs[0]["address"], a.address(&alice));
    assert_eq!(theirs[0]["provider"], "Organizer");
    assert_eq!(theirs[0]["organizer"], true);
    let (_, invites) = a.get("/api/invites/federated", &alice).await;
    assert_eq!(invites[0]["accepted_by"], b.address(&bob));
    assert!(invites[0]["invite"].is_null());

    // A second invite, accepted twice.
    let (_, invite) = a.post("/api/invites/federated", &alice, json!({})).await;
    let carol = b.user(&b_admin, "carol@b.test").await;
    let (status, _) = b
        .post(
            "/api/invites/federated/accept",
            &carol,
            json!({ "invite": invite["invite"] }),
        )
        .await;
    assert_eq!(status, 201);
    let (status, refused) = b
        .post(
            "/api/invites/federated/accept",
            &bob,
            json!({ "invite": invite["invite"] }),
        )
        .await;
    assert_eq!(status, 409, "{refused}");
    assert_eq!(refused["code"], "invite_used");
    // An unknown token.
    let fake = organizer::federation::address::invite_string("nope", &a.host);
    let (status, refused) = b
        .post(
            "/api/invites/federated/accept",
            &bob,
            json!({ "invite": fake }),
        )
        .await;
    assert_eq!(status, 400, "{refused}");
    assert_eq!(refused["code"], "invite_invalid");
    // Removing a contact.
    let id = mine[0]["id"].as_str().unwrap();
    let (status, _) = a
        .call(
            reqwest::Method::DELETE,
            &format!("/api/contacts/{id}"),
            Some(&alice),
            None,
        )
        .await;
    assert_eq!(status, 204);
    let (_, left) = a.get("/api/contacts", &alice).await;
    assert_eq!(left.as_array().unwrap().len(), 1);
    assert_eq!(left[0]["address"], b.address(&carol));
}

/// A contact's server type follows its discovery each time it is fetched
/// again: an Organizer server, or another OCM server by its `provider`.
#[tokio::test]
async fn a_contacts_server_is_refreshed_with_its_discovery() {
    let Pair {
        a, b, alice, bob, ..
    } = pair().await;
    let plain = plain_ocm_server().await;
    let alice_id: uuid::Uuid =
        sqlx::query_scalar("SELECT id FROM users WHERE email = 'alice@a.test'")
            .fetch_one(a.state.db.reader())
            .await
            .unwrap();
    // A contact on a plain OCM server, made before anything was known of
    // it; and bob's server, as if it had changed since.
    sqlx::query("INSERT INTO contacts (id, user_id, address, host) VALUES (?1, ?2, ?3, ?4)")
        .bind(uuid::Uuid::now_v7())
        .bind(alice_id)
        .bind(format!("eve@{plain}"))
        .bind(&plain)
        .execute(a.state.db.writer())
        .await
        .unwrap();
    sqlx::query("UPDATE contacts SET provider = 'Old', organizer = 0 WHERE host = ?1")
        .bind(&b.host)
        .execute(a.state.db.writer())
        .await
        .unwrap();
    let (_, before) = a.get("/api/contacts", &alice).await;
    let eve_address = format!("eve@{plain}");
    assert!(named_by(&before, "address", &eve_address)["provider"].is_null());
    assert_eq!(
        named_by(&before, "address", &eve_address)["organizer"],
        false
    );

    // Their discovery fetched again (the Share dialog's lookup does).
    a.state.files.federation().forget(&b.host);
    for address in [format!("eve@{plain}"), b.address(&bob)] {
        let query = address.replace('@', "%40");
        let (status, _) = a
            .get(&format!("/api/federation/lookup?address={query}"), &alice)
            .await;
        assert_eq!(status, 200);
    }
    // Noted in the background.
    let mut after = Value::Null;
    for _ in 0..50 {
        after = a.get("/api/contacts", &alice).await.1;
        let bob = named_by(&after, "server", &b.host);
        if named_by(&after, "address", &eve_address)["provider"] == "Nextcloud"
            && bob["organizer"] == true
        {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    }
    assert_eq!(
        named_by(&after, "address", &eve_address)["provider"],
        "Nextcloud"
    );
    assert_eq!(
        named_by(&after, "address", &eve_address)["organizer"],
        false
    );
    let bob = named_by(&after, "server", &b.host);
    assert_eq!(bob["provider"], "Organizer");
    assert_eq!(bob["organizer"], true);
}

/// The item of a list whose `field` is `value`.
fn named_by<'a>(items: &'a Value, field: &str, value: &str) -> &'a Value {
    items
        .as_array()
        .unwrap()
        .iter()
        .find(|i| i[field] == value)
        .unwrap_or_else(|| panic!("no {field} {value} in {items}"))
}

/// A folder shared with a contact on another server: it arrives pending in
/// "Shared with me", from its server; accepted, it is browsed, read and
/// (as an editor) written through bob's own server, and the roles hold.
#[tokio::test]
async fn a_folder_is_shared_read_and_written_with_its_role() {
    let p = pair().await;
    let (a, b, alice, bob) = (&p.a, &p.b, &p.alice, &p.bob);
    let root = a.root(alice).await;
    let docs = a.folder(alice, &root, "Docs").await;
    let (status, _) = a.upload(alice, &docs, "plan.txt", b"the plan").await;
    assert_eq!(status, 201);
    let (grant, item) = p.share(&docs, "editor").await;
    assert_eq!(grant["remote"]["address"], b.address(bob));
    assert_eq!(grant["remote"]["server"], b.host);
    assert_eq!(item["node"]["name"], "Docs");
    assert_eq!(item["remote"]["server"], a.host);
    assert_eq!(item["owner"]["email"], a.address(alice));
    assert_eq!(item["role"], "editor");
    // A sees it accepted.
    let (_, access) = a.get(&format!("/api/files/{docs}/access"), alice).await;
    assert_eq!(access["grants"][0]["remote"]["state"], "accepted");
    // And below it, inherited, with who and where as on the folder.
    let sub = a.folder(alice, &docs, "Sub").await;
    let (_, access) = a.get(&format!("/api/files/{sub}/access"), alice).await;
    let inherited = &access["inherited"][0];
    assert_eq!(inherited["from_name"], "Docs");
    assert_eq!(inherited["remote"]["address"], b.address(bob));
    assert_eq!(inherited["remote"]["server"], b.host);
    assert_eq!(inherited["remote"]["state"], "accepted");
    assert!(inherited["user"].is_null());

    let top = item["node"]["id"].as_str().unwrap();
    let listed = b.children(bob, top).await;
    let plan = named(&listed, "plan.txt");
    assert_eq!(plan["my_role"], "editor");
    assert_eq!(plan["remote"], a.host);
    let (status, bytes) = b.download(bob, plan["id"].as_str().unwrap()).await;
    assert_eq!(status, 200);
    assert_eq!(bytes, b"the plan");
    // Bob's server never shares it on itself (a public link of it).
    let (status, refused) = b
        .post(
            &format!("/api/files/{}/links", plan["id"].as_str().unwrap()),
            bob,
            json!({ "kind": "view" }),
        )
        .await;
    assert_eq!(status, 400, "{refused}");
    // Bob writes: it is on A.
    let (status, made) = b.upload(bob, top, "notes.txt", b"from bob").await;
    assert_eq!(status, 201, "{made}");
    a.state
        .files
        .scan(a.state.files.owner_of(docs.parse().unwrap()).await.unwrap())
        .await
        .unwrap();
    let on_a = a.children(alice, &docs).await;
    let notes = named(&on_a, "notes.txt");
    let (_, bytes) = a.download(alice, notes["id"].as_str().unwrap()).await;
    assert_eq!(bytes, b"from bob");

    // Viewer now: told to B, which refuses bob's writes.
    let share = grant["id"].as_str().unwrap();
    let (status, changed) = a
        .call(
            reqwest::Method::PATCH,
            &format!("/api/files/{docs}/access/{share}"),
            Some(alice),
            Some(json!({ "role": "viewer" })),
        )
        .await;
    assert_eq!(status, 200, "{changed}");
    let shared = b.shared_with_me(bob).await;
    assert_eq!(shared[0]["role"], "viewer");
    let (status, _) = b.upload(bob, top, "more.txt", b"no").await;
    assert_eq!(status, 403);
    // Upload only and owner are not offered for people elsewhere.
    for role in ["upload_only", "owner"] {
        let (status, _) = a
            .call(
                reqwest::Method::PATCH,
                &format!("/api/files/{docs}/access/{share}"),
                Some(alice),
                Some(json!({ "role": role })),
            )
            .await;
        assert!(status == 400 || status == 403, "{role}: {status}");
    }
}

/// What B keeps of a share from A: the secret (opened) and its WebDAV
/// URL and A's token endpoint.
async fn received_secret(b: &Server) -> (String, String, String) {
    let (secret, uri, endpoint): (Vec<u8>, String, String) = sqlx::query_as(
        "SELECT secret, uri, token_endpoint FROM ocm_received ORDER BY created_at DESC",
    )
    .fetch_one(b.state.db.reader())
    .await
    .unwrap();
    (
        b.state.files.backends().open(&secret).unwrap(),
        uri,
        endpoint,
    )
}

/// A WebDAV request to another server with a bearer token.
async fn dav(method: &str, url: &str, token: &str, depth: &str, body: &str) -> (u16, String) {
    let response = reqwest::Client::new()
        .request(reqwest::Method::from_bytes(method.as_bytes()).unwrap(), url)
        .bearer_auth(token)
        .header("depth", depth)
        .header("content-type", "application/xml")
        .body(body.to_string())
        .send()
        .await
        .unwrap();
    (response.status().as_u16(), response.text().await.unwrap())
}

fn sync_report(token: &str) -> String {
    format!(
        r#"<?xml version="1.0" encoding="utf-8"?><d:sync-collection xmlns:d="DAV:"><d:sync-token>{token}</d:sync-token><d:sync-level>1</d:sync-level><d:prop><d:getetag/></d:prop></d:sync-collection>"#
    )
}

/// The code flow (must-exchange-token): the secret itself opens nothing,
/// the token of a signed exchange does; the WebDAV follows the role and
/// limited access, and answers sync-collection.
#[tokio::test]
async fn the_code_flow_opens_the_webdav_as_the_role_allows() {
    let p = pair().await;
    let (a, b, alice) = (&p.a, &p.b, &p.alice);
    let root = a.root(alice).await;
    let docs = a.folder(alice, &root, "Docs").await;
    a.upload(alice, &docs, "a.txt", b"aaa").await;
    let private = a.folder(alice, &docs, "Private").await;
    a.upload(alice, &private, "s.txt", b"secret").await;
    p.share(&docs, "viewer").await;
    let (secret, uri, endpoint) = received_secret(b).await;
    assert!(uri.starts_with(&format!("{}/ocm/dav/", a.base)));

    // The secret is no bearer token: the share requires the exchange.
    let (status, _) = dav("PROPFIND", &uri, &secret, "1", "").await;
    assert_eq!(status, 401);
    let federation = b.state.files.federation();
    let exchange = |code: &str| {
        format!(
            "grant_type=authorization_code&client_id={}&code={}",
            organizer::util::form_encode(&b.host),
            organizer::util::form_encode(code)
        )
    };
    let (status, answer) = federation
        .post_signed(
            &endpoint,
            "application/x-www-form-urlencoded",
            exchange("wrong").into_bytes(),
        )
        .await
        .unwrap();
    assert_eq!(status, 400);
    assert_eq!(
        serde_json::from_slice::<Value>(&answer).unwrap()["error"],
        "invalid_grant"
    );
    let (status, answer) = federation
        .post_signed(
            &endpoint,
            "application/x-www-form-urlencoded",
            exchange(&secret).into_bytes(),
        )
        .await
        .unwrap();
    assert_eq!(status, 200);
    let answer: Value = serde_json::from_slice(&answer).unwrap();
    assert_eq!(answer["token_type"], "Bearer");
    assert_eq!(answer["expires_in"], 300);
    let token = answer["access_token"].as_str().unwrap().to_string();

    let (status, listing) = dav("PROPFIND", &uri, &token, "1", "").await;
    assert_eq!(status, 207, "{listing}");
    assert!(
        listing.contains("a.txt") && listing.contains("Private"),
        "{listing}"
    );
    let file = format!("{uri}/a.txt");
    let (status, content) = dav("GET", &file, &token, "0", "").await;
    assert_eq!((status, content.as_str()), (200, "aaa"));
    // A viewer writes nothing.
    let (status, _) = dav("PUT", &format!("{uri}/new.txt"), &token, "0", "x").await;
    assert_eq!(status, 403);
    let (status, _) = dav("DELETE", &file, &token, "0", "").await;
    assert_eq!(status, 403);

    // sync-collection: everything first, then what changed.
    let (status, first) = dav("REPORT", &format!("{uri}/"), &token, "0", &sync_report("")).await;
    assert_eq!(status, 207, "{first}");
    assert!(
        first.contains("a.txt") && first.contains("Private/"),
        "{first}"
    );
    let sync_token = |xml: &str| {
        let start = xml.find("<d:sync-token>").unwrap() + "<d:sync-token>".len();
        xml[start..start + xml[start..].find('<').unwrap()].to_string()
    };
    let t1 = sync_token(&first);
    let (_, b_txt) = a.upload(alice, &docs, "b.txt", b"bbb").await;
    let (status, second) = dav("REPORT", &format!("{uri}/"), &token, "0", &sync_report(&t1)).await;
    assert_eq!(status, 207);
    assert!(
        second.contains("b.txt") && !second.contains("a.txt"),
        "{second}"
    );
    let t2 = sync_token(&second);
    let id = b_txt["id"].as_str().unwrap();
    let version = b_txt["version"].as_i64().unwrap();
    let (status, _) = a
        .call(
            reqwest::Method::DELETE,
            &format!("/api/files/{id}?if_version={version}"),
            Some(alice),
            None,
        )
        .await;
    assert_eq!(status, 200);
    let (_, third) = dav("REPORT", &format!("{uri}/"), &token, "0", &sync_report(&t2)).await;
    assert!(
        third.contains("b.txt") && third.contains("404 Not Found"),
        "{third}"
    );
    let (status, invalid) = dav(
        "REPORT",
        &format!("{uri}/"),
        &token,
        "0",
        &sync_report("https://elsewhere/ocm/sync/x/1"),
    )
    .await;
    assert_eq!(status, 403);
    assert!(invalid.contains("valid-sync-token"));

    // Limited access on Private: gone for the share, as for a local user.
    let (status, _) = a
        .call(
            reqwest::Method::PATCH,
            &format!("/api/files/{private}/access"),
            Some(alice),
            Some(json!({ "limited": true })),
        )
        .await;
    assert_eq!(status, 200);
    let (_, listing) = dav("PROPFIND", &uri, &token, "1", "").await;
    assert!(!listing.contains("Private"), "{listing}");
    let (status, _) = dav("GET", &format!("{uri}/Private/s.txt"), &token, "0", "").await;
    assert_eq!(status, 404);
    let (_, fourth) = dav(
        "REPORT",
        &format!("{uri}/"),
        &token,
        "0",
        &sync_report(&sync_token(&third)),
    )
    .await;
    assert!(
        fourth.contains("Private") && fourth.contains("404"),
        "{fourth}"
    );
}

/// A share whose server stops answering says why (`remote.error`), keeps
/// following it by sync-collection, and the error goes once it answers.
#[tokio::test]
async fn a_share_whose_server_fails_says_why_until_it_answers() {
    let p = pair().await;
    let (a, b, alice, bob) = (&p.a, &p.b, &p.alice, &p.bob);
    let root = a.root(alice).await;
    let docs = a.folder(alice, &root, "Docs").await;
    p.share(&docs, "viewer").await;
    b.follow().await;
    let follow_all = || async {
        let ids: Vec<uuid::Uuid> = sqlx::query_scalar("SELECT id FROM ocm_received")
            .fetch_all(b.state.db.reader())
            .await
            .unwrap();
        for id in ids {
            // A failure is what is checked here.
            let _ = incoming::follow(&b.state.files, id).await;
        }
    };
    let error = || async {
        let shared = b.shared_with_me(bob).await;
        shared[0]["remote"]["error"].clone()
    };
    assert!(error().await.is_null());

    // A turns federation off: its WebDAV refuses the share.
    a.federation(&p.a_admin, json!({ "mode": "off" })).await;
    follow_all().await;
    let why = error().await;
    assert!(why.as_str().is_some_and(|w| !w.is_empty()), "{why}");
    // Still followed by sync-collection: a refusal is not "unsupported".
    let by_report: Option<bool> = sqlx::query_scalar("SELECT sync_collection FROM ocm_received")
        .fetch_one(b.state.db.reader())
        .await
        .unwrap();
    assert_eq!(by_report, Some(true));

    a.federation(&p.a_admin, json!({ "mode": "open" })).await;
    follow_all().await;
    assert!(error().await.is_null());
}

/// Unsharing and declining are told to the other server; a share placed
/// in My drive brings its changes to the change feed; a file is shared as
/// a file.
#[tokio::test]
async fn unshare_decline_place_and_follow() {
    let p = pair().await;
    let (a, b, alice, bob) = (&p.a, &p.b, &p.alice, &p.bob);
    let root = a.root(alice).await;
    let docs = a.folder(alice, &root, "Docs").await;
    let (grant, item) = p.share(&docs, "editor").await;
    let top = item["node"]["id"].as_str().unwrap().to_string();
    // Placed in bob's My drive: listed, and its changes reach his feed.
    let bob_root = b.root(bob).await;
    let (status, placed) = b
        .post(
            &format!("/api/shared/{top}/place"),
            bob,
            json!({ "parent_id": bob_root }),
        )
        .await;
    assert_eq!(status, 201, "{placed}");
    assert_eq!(placed["name"], "Docs");
    assert_eq!(placed["path"], "/Docs");
    let (_, cursor) = b.get("/api/changes", bob).await;
    let cursor = cursor["cursor"].as_str().unwrap().to_string();
    a.upload(alice, &docs, "new.txt", b"new").await;
    b.follow().await;
    let (_, changes) = b.get(&format!("/api/changes?cursor={cursor}"), bob).await;
    let names: Vec<&str> = changes["changes"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|c| c["name"].as_str())
        .collect();
    assert!(names.contains(&"new.txt"), "{changes}");

    // Unshared: gone on B.
    let share = grant["id"].as_str().unwrap();
    let (status, _) = a
        .call(
            reqwest::Method::DELETE,
            &format!("/api/files/{docs}/access/{share}"),
            Some(alice),
            None,
        )
        .await;
    assert_eq!(status, 204);
    assert!(b.shared_with_me(bob).await.is_empty());
    let listed = b.children(bob, &bob_root).await;
    assert!(listed.iter().all(|i| i["name"] != "Docs"), "{listed:?}");

    // A file, declined: A's grant says so.
    let (_, note) = a.upload(alice, &root, "note.txt", b"note").await;
    let note = note["id"].as_str().unwrap().to_string();
    let (status, grant) = a
        .post(
            &format!("/api/files/{note}/access"),
            alice,
            json!({ "address": b.address(bob), "role": "viewer" }),
        )
        .await;
    assert_eq!(status, 201, "{grant}");
    let shared = b.shared_with_me(bob).await;
    assert_eq!(shared[0]["node"]["name"], "note.txt");
    // Accepted, it is the drive's one file.
    let top = shared[0]["node"]["id"].as_str().unwrap();
    let (status, _) = b
        .post(&format!("/api/shared/{top}/accept"), bob, json!({}))
        .await;
    assert_eq!(status, 204);
    let listed = b.children(bob, top).await;
    let file = named(&listed, "note.txt");
    let (_, bytes) = b.download(bob, file["id"].as_str().unwrap()).await;
    assert_eq!(bytes, b"note");
    let (status, _) = b
        .call(
            reqwest::Method::DELETE,
            &format!("/api/shared/{top}"),
            Some(bob),
            None,
        )
        .await;
    assert_eq!(status, 204);
    assert!(b.shared_with_me(bob).await.is_empty());
    let (_, access) = a.get(&format!("/api/files/{note}/access"), alice).await;
    assert_eq!(access["grants"][0]["remote"]["state"], "declined");

    // A shared folder trashed: the share ends, and B is told.
    let gone = a.folder(alice, &root, "Gone").await;
    p.share(&gone, "viewer").await;
    assert_eq!(b.shared_with_me(bob).await.len(), 1);
    let (_, node) = a.get(&format!("/api/files/{gone}"), alice).await;
    let version = node["version"].as_i64().unwrap();
    let (status, _) = a
        .call(
            reqwest::Method::DELETE,
            &format!("/api/files/{gone}?if_version={version}"),
            Some(alice),
            None,
        )
        .await;
    assert_eq!(status, 200);
    let mut ended = false;
    for _ in 0..50 {
        tokio::time::sleep(Duration::from_millis(100)).await;
        if b.shared_with_me(bob).await.is_empty() {
            ended = true;
            break;
        }
    }
    assert!(ended, "B was told the share ended");
}

/// Contacts first: sharing with someone who is not a contact is refused,
/// unless their server is listed or `federation.share_with_any` allows any
/// address (on both sides: the recipient's server takes shares from
/// contacts too).
#[tokio::test]
async fn contacts_first_unless_listed_or_any_address() {
    let p = pair().await;
    let (a, b, alice) = (&p.a, &p.b, &p.alice);
    let carol = b.user(&p.b_admin, "carol@b.test").await;
    let root = a.root(alice).await;
    let docs = a.folder(alice, &root, "Docs").await;
    let share = |to: String| {
        let docs = docs.clone();
        async move {
            a.post(
                &format!("/api/files/{docs}/access"),
                alice,
                json!({ "address": to, "role": "viewer" }),
            )
            .await
        }
    };
    let (status, refused) = share(b.address(&carol)).await;
    assert_eq!(status, 403, "{refused}");
    assert_eq!(refused["code"], "contact_required");
    let (_, lookup) = a
        .get(
            &format!(
                "/api/federation/lookup?address={}",
                organizer::util::form_encode(&b.address(&carol))
            ),
            alice,
        )
        .await;
    assert_eq!(lookup["may_share"], false);
    assert_eq!(lookup["refusal"], "contact_required");
    // Any address, on both servers.
    a.setting(&p.a_admin, "federation.share_with_any", json!(true))
        .await;
    b.setting(&p.b_admin, "federation.share_with_any", json!(true))
        .await;
    let (status, grant) = share(b.address(&carol)).await;
    assert_eq!(status, 201, "{grant}");
    assert_eq!(b.shared_with_me(&carol).await.len(), 1);
    // Listed servers count as trusted, contacts or not.
    a.setting(&p.a_admin, "federation.share_with_any", json!(false))
        .await;
    b.setting(&p.b_admin, "federation.share_with_any", json!(false))
        .await;
    let dave = b.user(&p.b_admin, "dave@b.test").await;
    let (status, _) = share(b.address(&dave)).await;
    assert_eq!(status, 403);
    a.federation(&p.a_admin, json!({ "mode": "listed", "allowed": [b.host] }))
        .await;
    b.federation(&p.b_admin, json!({ "mode": "listed", "allowed": [a.host] }))
        .await;
    let (status, grant) = share(b.address(&dave)).await;
    assert_eq!(status, 201, "{grant}");
    let (_, lookup) = a
        .get(
            &format!(
                "/api/federation/lookup?address={}",
                organizer::util::form_encode(&b.address(&dave))
            ),
            alice,
        )
        .await;
    assert_eq!(lookup["listed"], true);
    assert_eq!(lookup["may_share"], true);
    assert_eq!(lookup["drives"], true);
}

/// Off, listed and open with a server refused: refused servers get
/// nothing, either way, and their tokens stop.
#[tokio::test]
async fn the_modes_and_refused_servers() {
    let p = pair().await;
    let (a, b, alice, bob) = (&p.a, &p.b, &p.alice, &p.bob);
    let root = a.root(alice).await;
    let docs = a.folder(alice, &root, "Docs").await;
    p.share(&docs, "viewer").await;
    // A refuses B: alice cannot share with bob, and B's access stops.
    a.federation(&p.a_admin, json!({ "mode": "open", "refused": [b.host] }))
        .await;
    let other = a.folder(alice, &root, "Other").await;
    let (status, refused) = a
        .post(
            &format!("/api/files/{other}/access"),
            alice,
            json!({ "address": b.address(bob), "role": "viewer" }),
        )
        .await;
    assert_eq!(status, 403, "{refused}");
    assert_eq!(refused["code"], "server_refused");
    let (secret, _, endpoint) = received_secret(b).await;
    let body = format!(
        "grant_type=authorization_code&client_id={}&code={}",
        organizer::util::form_encode(&b.host),
        organizer::util::form_encode(&secret)
    );
    let (status, answer) = b
        .state
        .files
        .federation()
        .post_signed(
            &endpoint,
            "application/x-www-form-urlencoded",
            body.clone().into_bytes(),
        )
        .await
        .unwrap();
    assert_eq!(status, 400);
    assert_eq!(
        serde_json::from_slice::<Value>(&answer).unwrap()["error"],
        "unauthorized_client"
    );
    // B's people cannot share with A's either (A refuses them on receipt).
    let bobs = b.folder(bob, &b.root(bob).await, "Bobs").await;
    let (status, refused) = b
        .post(
            &format!("/api/files/{bobs}/access"),
            bob,
            json!({ "address": a.address(alice), "role": "viewer" }),
        )
        .await;
    assert_eq!(status, 502, "{refused}");
    assert_eq!(refused["code"], "remote_refused");
    // Listed, without B: the same.
    a.federation(
        &p.a_admin,
        json!({ "mode": "listed", "allowed": ["c.example"] }),
    )
    .await;
    let (status, _) = b
        .state
        .files
        .federation()
        .post_signed(
            &endpoint,
            "application/x-www-form-urlencoded",
            body.clone().into_bytes(),
        )
        .await
        .unwrap();
    assert_eq!(status, 400);
    // Off: discovery says so, everything stops.
    a.federation(&p.a_admin, json!({ "mode": "off" })).await;
    let (status, refused) = a
        .post(
            &format!("/api/files/{other}/access"),
            alice,
            json!({ "address": b.address(bob), "role": "viewer" }),
        )
        .await;
    assert_eq!(status, 403);
    assert_eq!(refused["code"], "federation_off");
    // Open again: the share works again.
    a.federation(&p.a_admin, json!({ "mode": "open" })).await;
    let (status, _) = b
        .state
        .files
        .federation()
        .post_signed(
            &endpoint,
            "application/x-www-form-urlencoded",
            body.into_bytes(),
        )
        .await
        .unwrap();
    assert_eq!(status, 200);
}

/// A drive's managers may forbid sharing it with other servers
/// (`federation.allowed`): new shares are refused, existing ones stop.
#[tokio::test]
async fn a_drive_may_forbid_federation() {
    let p = pair().await;
    let (a, b, alice, bob) = (&p.a, &p.b, &p.alice, &p.bob);
    let root = a.root(alice).await;
    let docs = a.folder(alice, &root, "Docs").await;
    p.share(&docs, "viewer").await;
    let (_, drives) = a.get("/api/drives", alice).await;
    let my = drives[0]["id"].as_str().unwrap();
    let (status, set) = a
        .call(
            reqwest::Method::PUT,
            &format!("/api/drives/{my}/settings/federation.allowed"),
            Some(alice),
            Some(json!({ "mode": "default", "value": false })),
        )
        .await;
    assert_eq!(status, 200, "{set}");
    let other = a.folder(alice, &root, "Other").await;
    let (status, refused) = a
        .post(
            &format!("/api/files/{other}/access"),
            alice,
            json!({ "address": b.address(bob), "role": "viewer" }),
        )
        .await;
    assert_eq!(status, 403, "{refused}");
    assert_eq!(refused["code"], "federation_forbidden");
    let (secret, _, endpoint) = received_secret(b).await;
    let body = format!(
        "grant_type=authorization_code&client_id={}&code={}",
        organizer::util::form_encode(&b.host),
        organizer::util::form_encode(&secret)
    );
    let (status, _) = b
        .state
        .files
        .federation()
        .post_signed(
            &endpoint,
            "application/x-www-form-urlencoded",
            body.into_bytes(),
        )
        .await
        .unwrap();
    assert_eq!(status, 400);
}

/// A stand-in OCM server without our extensions: its discovery only.
async fn plain_ocm_server() -> String {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let base = format!("http://{addr}");
    let document = json!({
        "enabled": true,
        "apiVersion": "1.2.0",
        "endPoint": format!("{base}/ocm"),
        "provider": "Nextcloud",
        "resourceTypes": [
            { "name": "file", "shareTypes": ["user"], "protocols": { "webdav": "/dav/", "webdav-receive": {} } },
            { "name": "folder", "shareTypes": ["user"], "protocols": { "webdav": "/dav/", "webdav-receive": {} } },
        ],
        "capabilities": ["http-sig", "exchange-token"],
        "jwksUri": format!("{base}/ocm/jwks"),
        "tokenEndPoint": format!("{base}/ocm/token"),
    });
    let router = axum::Router::new().route(
        "/.well-known/ocm",
        axum::routing::get(move || {
            let document = document.clone();
            async move { axum::Json(document) }
        }),
    );
    tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });
    addr.to_string()
}

/// Our extensions between Organizer servers: a whole shared drive with a
/// role (in bob's drives), an encrypted folder only to a server with
/// `organizer-e2e`, and live notices that bring a change at once.
#[tokio::test]
async fn the_extensions_between_organizer_servers() {
    let p = pair().await;
    let (a, b, alice, bob) = (&p.a, &p.b, &p.alice, &p.bob);
    // A whole drive.
    let files = &a.state.files;
    let alice_id: uuid::Uuid =
        sqlx::query_scalar("SELECT id FROM users WHERE email = 'alice@a.test'")
            .fetch_one(a.state.db.reader())
            .await
            .unwrap();
    let team = files
        .create_shared_drive("Team", None, alice_id)
        .await
        .unwrap();
    files.set_member(team, alice_id, true).await.unwrap();
    let team_root = files.root_node(team).await.unwrap().id.to_string();
    a.upload(alice, &team_root, "minutes.txt", b"minutes").await;
    let (status, grant) = a
        .post(
            &format!("/api/drives/{team}/access"),
            alice,
            json!({ "address": b.address(bob), "role": "editor" }),
        )
        .await;
    assert_eq!(status, 201, "{grant}");
    let (_, access) = a.get(&format!("/api/drives/{team}/access"), alice).await;
    assert!(
        access["grants"]
            .as_array()
            .unwrap()
            .iter()
            .any(|g| g["remote"]["address"] == b.address(bob))
    );
    let (_, drives) = b.get("/api/drives", bob).await;
    let remote = drives
        .as_array()
        .unwrap()
        .iter()
        .find(|d| d["kind"] == "remote")
        .cloned()
        .expect("the drive among bob's");
    assert_eq!(remote["name"], "Team");
    assert_eq!(remote["my_role"], "editor");
    assert_eq!(remote["remote"]["state"], "pending");
    assert_eq!(remote["remote"]["server"], a.host);
    let remote_root = remote["root_id"].as_str().unwrap();
    let (status, _) = b
        .post(&format!("/api/shared/{remote_root}/accept"), bob, json!({}))
        .await;
    assert_eq!(status, 204);
    let listed = b.children(bob, remote_root).await;
    assert_eq!(named(&listed, "minutes.txt")["my_role"], "editor");

    // Live: a change on A reaches B without waiting for a look.
    a.upload(alice, &team_root, "agenda.txt", b"agenda").await;
    let mut seen = false;
    for _ in 0..50 {
        tokio::time::sleep(Duration::from_millis(100)).await;
        let listed = b.children(bob, remote_root).await;
        if listed.iter().any(|i| i["name"] == "agenda.txt") {
            seen = true;
            break;
        }
    }
    assert!(seen, "the live notice brought the change");

    // An encrypted folder: never to a server without organizer-e2e.
    let plain = plain_ocm_server().await;
    a.setting(&p.a_admin, "federation.share_with_any", json!(true))
        .await;
    let root = a.root(alice).await;
    let alice_root: uuid::Uuid = root.parse().unwrap();
    let secret = files
        .create_encrypted_folder(alice_id, alice_root, "Secret")
        .await
        .unwrap();
    let (status, refused) = a
        .post(
            &format!("/api/files/{}/access", secret.id),
            alice,
            json!({ "address": format!("eve@{plain}"), "role": "viewer" }),
        )
        .await;
    assert_eq!(status, 400, "{refused}");
    assert_eq!(refused["code"], "e2e_unsupported");
    // A whole drive neither.
    let (status, refused) = a
        .post(
            &format!("/api/drives/{team}/access"),
            alice,
            json!({ "address": format!("eve@{plain}"), "role": "viewer" }),
        )
        .await;
    assert_eq!(status, 400, "{refused}");
    assert_eq!(refused["code"], "drives_unsupported");
    // To B (organizer-e2e): shared, marked encrypted on both sides.
    let (status, grant) = a
        .post(
            &format!("/api/files/{}/access", secret.id),
            alice,
            json!({ "address": b.address(bob), "role": "viewer" }),
        )
        .await;
    assert_eq!(status, 201, "{grant}");
    let e2e: bool = sqlx::query_scalar("SELECT e2e FROM ocm_received WHERE name = 'Secret'")
        .fetch_one(b.state.db.reader())
        .await
        .unwrap();
    assert!(e2e);
    // Encrypted on B too: sealed, opened only by its members' devices.
    let shared = b.shared_with_me(bob).await;
    let sealed = shared
        .iter()
        .find(|s| s["node"]["name"] == "Secret")
        .expect("the encrypted folder");
    assert_eq!(sealed["node"]["e2e_root"], sealed["node"]["id"]);
}

/// A stand-in for a device's encryption set-up: a public key of `seed`
/// bytes and wrappings of the right shapes (the server opens none).
fn set_up_body(seed: u8) -> Value {
    let b64 = |bytes: &[u8]| organizer::util::base64url_encode(bytes);
    let mut wrapped = vec![seed; 61];
    wrapped[0] = 1;
    json!({
        "public_key": b64(&[seed; 32]),
        "passphrase": { "salt": b64(&[seed; 16]), "iterations": 600_000, "wrapped": b64(&wrapped) },
        "recovery_wrapped": b64(&wrapped),
    })
}

/// A stand-in for a folder key wrapped for a member (docs/E2E_SPEC.md §4):
/// 93 bytes, version 1.
fn wrapped_key(seed: u8) -> String {
    let mut wrapped = vec![seed; 93];
    wrapped[0] = 1;
    organizer::util::base64url_encode(&wrapped)
}

async fn user_id(server: &Server, session: &Session) -> String {
    let (_, me) = server.get("/api/auth/session", session).await;
    me["user"]["id"].as_str().unwrap().to_string()
}

/// `organizer-e2e`: an encrypted folder shared with bob on B. His public
/// key comes from B, alice's device wraps the folder's keys for him on A
/// (he is a member there, marked remote), B hands them to him with A's
/// folder id, and B's index carries A's ids as `source_id`, so his device
/// opens names as in a mirror. Removing him drops his keys, and the next
/// generation is made without him.
#[tokio::test]
async fn an_encrypted_folder_is_shared_with_a_member_on_another_server() {
    let p = pair().await;
    let (a, b, alice, bob) = (&p.a, &p.b, &p.alice, &p.bob);
    let (status, _) = a.post("/api/encryption", alice, set_up_body(7)).await;
    assert_eq!(status, 201);
    let (status, _) = b.post("/api/encryption", bob, set_up_body(9)).await;
    assert_eq!(status, 201);
    let bob_key = organizer::util::base64url_encode(&[9u8; 32]);
    let alice_id = user_id(a, alice).await;
    let root = a.root(alice).await;
    let (status, secret) = a
        .post(
            &format!("/api/files/{root}/folders"),
            alice,
            json!({ "name": "Secret", "encrypted": true }),
        )
        .await;
    assert_eq!(status, 201, "{secret}");
    let secret = secret["id"].as_str().unwrap().to_string();
    let (status, keys) = a
        .post(
            &format!("/api/encryption/folders/{secret}/keys"),
            alice,
            json!({ "keys": [{ "user_id": alice_id, "generation": 1, "wrapped": wrapped_key(1) }] }),
        )
        .await;
    assert_eq!(status, 200, "{keys}");
    // Sealed names (opaque to the servers), a folder and a file in it.
    let inner = a.folder(alice, &secret, "AQAAAAFpbm5lci1mb2xkZXI").await;
    let (status, file) = a
        .upload(alice, &inner, "AQAAAAFzZWFsZWQtZmlsZQ", b"sealed bytes")
        .await;
    assert_eq!(status, 201, "{file}");
    let file = file["id"].as_str().unwrap().to_string();

    // Shared with bob: his public key came from B at once.
    let (grant, item) = p.share(&secret, "editor").await;
    let share = grant["id"].as_str().unwrap().to_string();
    let (status, keys) = a
        .get(&format!("/api/encryption/folders/{secret}"), alice)
        .await;
    assert_eq!(status, 200, "{keys}");
    let member = keys["members"]
        .as_array()
        .unwrap()
        .iter()
        .find(|m| m["remote"] == b.address(bob))
        .cloned()
        .expect("bob, a member on another server");
    assert_eq!(member["user_id"], share);
    assert_eq!(member["public_key"], bob_key);
    assert_eq!(member["missing"], json!([1]));
    let (_, pending) = a.get("/api/encryption/pending", alice).await;
    let task = pending
        .as_array()
        .unwrap()
        .iter()
        .find(|t| t["folder_id"] == secret)
        .cloned()
        .expect("bob waits for a key");
    assert_eq!(task["waiting"], json!([share]));
    // Alice's device wraps it for him (kept on A).
    let (status, keys) = a
        .post(
            &format!("/api/encryption/folders/{secret}/keys"),
            alice,
            json!({ "keys": [{ "user_id": share, "generation": 1, "wrapped": wrapped_key(2) }] }),
        )
        .await;
    assert_eq!(status, 200, "{keys}");
    let (_, pending) = a.get("/api/encryption/pending", alice).await;
    assert!(
        pending
            .as_array()
            .unwrap()
            .iter()
            .all(|t| t["folder_id"] != secret),
        "{pending}"
    );

    // On B: bob's keys from A, with A's folder id; the index has A's ids.
    let top = item["node"]["id"].as_str().unwrap().to_string();
    let (status, keys) = b.get(&format!("/api/encryption/folders/{top}"), bob).await;
    assert_eq!(status, 200, "{keys}");
    assert_eq!(keys["folder_id"], secret);
    assert_eq!(keys["generation"], 1);
    assert_eq!(
        keys["keys"],
        json!([{ "generation": 1, "wrapped": wrapped_key(2) }])
    );
    assert_eq!(keys["members"], json!([]));
    assert_eq!(keys["needs_new_key"], false);
    // Asked by the owner's id (`source_id`) too, as a mirror's client asks.
    let (status, by_source) = b
        .get(&format!("/api/encryption/folders/{secret}"), bob)
        .await;
    assert_eq!(status, 200, "{by_source}");
    assert_eq!(by_source["folder_id"], secret);
    b.follow().await;
    let (_, top_node) = b.get(&format!("/api/files/{top}"), bob).await;
    assert_eq!(top_node["source_id"], secret);
    let listed = b.children(bob, &top).await;
    let folder = named(&listed, "AQAAAAFpbm5lci1mb2xkZXI");
    assert_eq!(folder["source_id"], inner);
    assert_eq!(folder["e2e_root"], top);
    let listed = b.children(bob, folder["id"].as_str().unwrap()).await;
    let sealed = named(&listed, "AQAAAAFzZWFsZWQtZmlsZQ");
    assert_eq!(sealed["source_id"], file);
    let (_, bytes) = b.download(bob, sealed["id"].as_str().unwrap()).await;
    assert_eq!(bytes, b"sealed bytes");
    // A folder bob makes in it has its id on A at once: what goes in it is
    // sealed with that.
    let (status, made) = b
        .post(
            &format!("/api/files/{top}/folders"),
            bob,
            json!({ "name": "AQAAAAFib2ItZm9sZGVy" }),
        )
        .await;
    assert_eq!(status, 201, "{made}");
    let on_a = a.children(alice, &secret).await;
    assert_eq!(
        made["source_id"],
        named(&on_a, "AQAAAAFib2ItZm9sZGVy")["id"],
        "{made}"
    );
    // Bob's device has nothing to do on B: the owner's devices see to it.
    let (_, pending) = b.get("/api/encryption/pending", bob).await;
    assert_eq!(pending, json!([]));

    // The new endpoints answer only signed requests.
    let (status, _) = a
        .call(
            reqwest::Method::POST,
            "/ocm/organizer-e2e/keys",
            None,
            Some(json!({ "senderDomain": b.host, "providerId": share })),
        )
        .await;
    assert_eq!(status, 401);

    // Removed: his keys go, the folder needs a new key, which is made
    // without him; B no longer has the folder or its keys.
    let (status, _) = a
        .call(
            reqwest::Method::DELETE,
            &format!("/api/files/{secret}/access/{share}"),
            Some(alice),
            None,
        )
        .await;
    assert_eq!(status, 204);
    let held: i64 = sqlx::query_scalar("SELECT count(*) FROM e2e_remote_keys")
        .fetch_one(a.state.db.reader())
        .await
        .unwrap();
    assert_eq!(held, 0);
    let (_, keys) = a
        .get(&format!("/api/encryption/folders/{secret}"), alice)
        .await;
    assert_eq!(keys["needs_new_key"], true);
    assert!(
        keys["members"]
            .as_array()
            .unwrap()
            .iter()
            .all(|m| m["remote"].is_null()),
        "{keys}"
    );
    let (status, keys) = a
        .post(
            &format!("/api/encryption/folders/{secret}/keys"),
            alice,
            json!({ "keys": [{ "user_id": alice_id, "generation": 2, "wrapped": wrapped_key(3) }] }),
        )
        .await;
    assert_eq!(status, 200, "{keys}");
    assert_eq!(keys["generation"], 2);
    let (status, _) = a
        .post(
            &format!("/api/encryption/folders/{secret}/keys"),
            alice,
            json!({ "keys": [{ "user_id": share, "generation": 2, "wrapped": wrapped_key(4) }] }),
        )
        .await;
    assert_eq!(status, 400, "no key for someone who is no longer a member");
    let (status, _) = b.get(&format!("/api/encryption/folders/{top}"), bob).await;
    assert_eq!(status, 404);
}

/// A server without `organizer-e2e` never gets an encrypted folder, so
/// none of its people is ever a member.
#[tokio::test]
async fn an_encrypted_folder_is_refused_to_a_server_without_organizer_e2e() {
    let p = pair().await;
    let (a, alice) = (&p.a, &p.alice);
    a.setting(&p.a_admin, "federation.share_with_any", json!(true))
        .await;
    let (status, _) = a.post("/api/encryption", alice, set_up_body(7)).await;
    assert_eq!(status, 201);
    let root = a.root(alice).await;
    let (_, secret) = a
        .post(
            &format!("/api/files/{root}/folders"),
            alice,
            json!({ "name": "Secret", "encrypted": true }),
        )
        .await;
    let secret = secret["id"].as_str().unwrap().to_string();
    let plain = plain_ocm_server().await;
    let (status, refused) = a
        .post(
            &format!("/api/files/{secret}/access"),
            alice,
            json!({ "address": format!("eve@{plain}"), "role": "viewer" }),
        )
        .await;
    assert_eq!(status, 400, "{refused}");
    assert_eq!(refused["code"], "e2e_unsupported");
    let (_, keys) = a
        .get(&format!("/api/encryption/folders/{secret}"), alice)
        .await;
    assert!(
        keys["members"]
            .as_array()
            .unwrap()
            .iter()
            .all(|m| m["remote"].is_null()),
        "{keys}"
    );
}

/// A share from another server brings its recipient a push notice (as a
/// local share does); a user removed is told to the servers of their
/// contacts and shares (`USER_REMOVED`), which drop the contact and end
/// the shares with them.
#[tokio::test]
async fn a_share_notifies_and_a_removed_user_is_told() {
    let p = pair().await;
    let (a, b, alice, bob) = (&p.a, &p.b, &p.alice, &p.bob);
    let root = a.root(alice).await;
    let docs = a.folder(alice, &root, "Docs").await;
    let (status, grant) = a
        .post(
            &format!("/api/files/{docs}/access"),
            alice,
            json!({ "address": b.address(bob), "role": "viewer" }),
        )
        .await;
    assert_eq!(status, 201, "{grant}");
    let mut noticed = Value::Null;
    for _ in 0..50 {
        let (_, news) = b.get("/api/notifications/news", bob).await;
        if news["notices"].as_array().is_some_and(|n| !n.is_empty()) {
            noticed = news;
            break;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    assert_eq!(noticed["notices"][0]["kind"], "shared", "{noticed}");

    // Bob is removed on B: A drops him as alice's contact and ends the
    // share with him.
    let bob_id = user_id(b, bob).await;
    let (status, answer) = b
        .call(
            reqwest::Method::DELETE,
            &format!("/api/superadmin/users/{bob_id}"),
            Some(&p.b_admin),
            None,
        )
        .await;
    assert_eq!(status, 204, "{answer}");
    let mut told = false;
    for _ in 0..50 {
        tokio::time::sleep(Duration::from_millis(100)).await;
        let (_, contacts) = a.get("/api/contacts", alice).await;
        let (_, access) = a.get(&format!("/api/files/{docs}/access"), alice).await;
        if contacts.as_array().is_some_and(|c| c.is_empty())
            && access["grants"].as_array().is_some_and(|g| g.is_empty())
        {
            told = true;
            break;
        }
    }
    assert!(told, "A was told bob was removed");
}

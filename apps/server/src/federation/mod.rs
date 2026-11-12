//! Federation through Open Cloud Mesh (docs/M7E_DESIGN.md), following
//! draft-ietf-ocm-open-cloud-mesh-07: discovery (`/.well-known/ocm`) and
//! WebFinger, the server's Ed25519 signing key published as a JWK Set,
//! HTTP Message Signatures on every request between servers, invites and
//! contacts, shares in both directions with the code flow's short-lived
//! tokens, and notifications.
//!
//! - **Out** (`outgoing.rs`): a file, a folder or (our extension) a whole
//!   shared drive is shared with `user@host` as a grant would be, with a
//!   role; the other server reaches it through our WebDAV at
//!   `/ocm/dav/<share>/` (`dav.rs`) with a token of our code flow, where
//!   the roles, limited access and the drive's availability hold as for a
//!   local user.
//! - **In** (`incoming.rs`): a share from another server is a drive of
//!   its own (`kind` `remote`) whose storage is that server's WebDAV with
//!   a token of its code flow; its recipient has a grant on its root (or on
//!   the drive), so it is in "Shared with me", can be placed in My drive,
//!   is browsed, read and written through our API like a local share, and
//!   its changes, followed by sync-collection or by its ETag, reach the
//!   change feed through the same reconciler as every drive.
//! - **Between Organizer servers** (§6): `organizer-drives` (whole drives
//!   with a role), `organizer-e2e` (encrypted folders, never shared with a
//!   server without it), `organizer-live` (a signed notice at each change).
//!
//! Nothing of a remote request is logged beyond what a local one is: no
//! secret, token, address or name.

pub mod address;
pub mod dav;
pub mod e2e;
pub mod incoming;
pub mod outgoing;
pub mod sign;

use crate::db::Db;
use address::host_of;
use serde::{Deserialize, Serialize};
use sign::Jwk;
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};
use utoipa::ToSchema;
use uuid::Uuid;

/// The OCM API version this server answers (draft-ietf-ocm-open-cloud-mesh-07).
pub const API_VERSION: &str = "1.2.0";

/// Our extensions, advertised in discovery and used only when both sides
/// have them (§6).
pub const DRIVES: &str = "organizer-drives";
pub const E2E: &str = "organizer-e2e";
pub const LIVE: &str = "organizer-live";

/// The OCM notification type of our live notices (§6): something changed
/// in a share. Only sent to servers with `organizer-live`.
pub const CHANGED: &str = "ORGANIZER_CHANGED";

/// The link relation of an account's OCM discovery in WebFinger: OCM
/// registers none, so the specification's URI names it (an extension
/// relation type, RFC 8288 §2.1.2).
pub const WEBFINGER_REL: &str = "https://datatracker.ietf.org/doc/draft-ietf-ocm-open-cloud-mesh/";

/// Where the OCM API is (`endPoint`), and our WebDAV for the shares.
pub const OCM_PREFIX: &str = "/ocm";
pub const DAV_PREFIX: &str = "/ocm/dav";

/// How long a request to another server may take.
const TIMEOUT: Duration = Duration::from_secs(30);
/// The most a server's JSON answer (discovery, JWK Set, a share's answer)
/// may be.
const MAX_ANSWER: usize = 1024 * 1024;
/// How long another server's discovery and keys are kept.
const KEEP_DISCOVERY: Duration = Duration::from_secs(300);
/// How long a failed discovery is not asked again.
const KEEP_FAILURE: Duration = Duration::from_secs(30);
/// How long a token of our code flow lasts (`expires_in`).
pub const TOKEN_SECS: u64 = 300;

/// The superadmin's choice (§2).
#[derive(
    Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize, ToSchema, sqlx::Type,
)]
#[serde(rename_all = "snake_case")]
#[sqlx(type_name = "TEXT", rename_all = "snake_case")]
#[schema(as = FederationMode)]
pub enum Mode {
    /// No federation: discovery says `enabled: false`, every OCM endpoint
    /// refuses.
    #[default]
    Off,
    /// Only the servers listed (`allowed`), which count as trusted.
    Listed,
    /// Any server but those refused.
    Open,
}

/// The server-wide federation settings, as stored in `server_settings`.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Settings {
    /// The superadmin's choice (§2).
    pub mode: Mode,
}

/// What we make of another server.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Trust {
    /// Federation is off, or the server is refused or not listed.
    Refused,
    /// Allowed; `listed`: in the superadmin's list (trusted: no contact
    /// needed to share with its people).
    Allowed { listed: bool },
}

/// Another server's discovery (§5.3), as much of it as we use.
#[derive(Clone, Debug, Default, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Discovery {
    #[serde(default)]
    pub enabled: bool,
    #[serde(default)]
    pub api_version: String,
    #[serde(default)]
    pub end_point: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider: Option<String>,
    #[serde(default)]
    pub resource_types: Vec<ResourceType>,
    #[serde(default)]
    pub capabilities: Vec<String>,
    #[serde(default)]
    pub criteria: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub invite_accept_dialog: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub jwks_uri: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub token_end_point: Option<String>,
}

#[derive(Clone, Debug, Default, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ResourceType {
    pub name: String,
    #[serde(default)]
    pub share_types: Vec<String>,
    #[serde(default)]
    pub protocols: serde_json::Map<String, serde_json::Value>,
}

impl Discovery {
    pub fn has(&self, capability: &str) -> bool {
        self.capabilities.iter().any(|c| c == capability)
    }

    pub fn requires(&self, criterion: &str) -> bool {
        self.criteria.iter().any(|c| c == criterion)
    }

    /// The resource type of that name.
    pub fn resource(&self, name: &str) -> Option<&ResourceType> {
        self.resource_types.iter().find(|r| r.name == name)
    }

    /// Whether the server takes a share of this resource type to a user
    /// over WebDAV (§7: the resource type, `user`, `webdav-receive`). A
    /// `file` over WebDAV may be assumed (backwards compatibility).
    pub fn receives(&self, resource_type: &str) -> bool {
        match self.resource(resource_type) {
            Some(r) => {
                r.share_types.iter().any(|s| s == "user")
                    && (r.protocols.contains_key("webdav-receive")
                        || r.protocols.contains_key("webdav"))
            }
            None => resource_type == "file",
        }
    }

    /// The prefix of its WebDAV for the shares it sends (§9 step 1).
    pub fn webdav_path(&self) -> Option<&str> {
        ["folder", "file"]
            .iter()
            .filter_map(|n| self.resource(n))
            .chain(self.resource_types.first())
            .find_map(|r| r.protocols.get("webdav").and_then(|v| v.as_str()))
    }

    /// Both servers are Organizer servers with this extension.
    pub fn extension(&self, name: &str) -> bool {
        self.has(name)
    }

    /// An Organizer server: it advertises one of our extensions.
    pub fn organizer(&self) -> bool {
        [DRIVES, E2E, LIVE].iter().any(|c| self.has(c))
    }

    /// The software it says it is (`provider`, "Nextcloud"), for people:
    /// plain characters, at most 64; none when it names none.
    pub fn provider_name(&self) -> Option<String> {
        let name: String = self
            .provider
            .as_deref()?
            .chars()
            .filter(|c| !c.is_control())
            .take(64)
            .collect();
        let name = name.trim();
        (!name.is_empty()).then(|| name.to_string())
    }
}

/// Federation's state of this server: the signing key, the client to
/// other servers and what is kept of them, and the tokens of the code
/// flow.
pub struct Federation {
    db: Db,
    data_dir: PathBuf,
    /// `OCM_KEY_FILE`: the signing key is read from there (never made).
    key_file: Mutex<Option<PathBuf>>,
    key: OnceLock<Key>,
    /// `FEDERATION_ALLOW_PRIVATE`: other servers may be at private and
    /// local addresses, and over http (tests, private networks).
    allow_private: AtomicBool,
    /// `PUBLIC_URL`: our address; without it federation stays off.
    public_url: Mutex<Option<String>>,
    clients: OnceLock<(reqwest::Client, reqwest::Client)>,
    discovery: Kept<Result<Arc<Discovery>, String>>,
    jwks: Kept<Arc<Vec<Jwk>>>,
    /// Tokens of our code flow, by their SHA-256: the share, and until
    /// when. In memory only (§12.4: never persisted beyond their life).
    issued: Mutex<HashMap<String, (Uuid, Instant)>>,
    /// Tokens other servers gave us, by the incoming share.
    obtained: Mutex<HashMap<Uuid, (String, Instant)>>,
    /// When someone last opened each incoming share: followed sooner then.
    pub(crate) opened: Mutex<HashMap<Uuid, Instant>>,
    /// The last row of `ocm_changes` whose live notices were sent.
    pub(crate) live_mark: std::sync::atomic::AtomicI64,
    /// Whether each share from another server answers, by its drive.
    healths: Mutex<HashMap<Uuid, Arc<crate::storage::Health>>>,
    /// When each share's member was last asked for their public key
    /// (`e2e::public_key`), while their server had none to give.
    pub(crate) keys_asked: Mutex<HashMap<Uuid, Instant>>,
}

/// What is kept of other servers, by host or URL, and since when.
type Kept<T> = Mutex<HashMap<String, (Instant, T)>>;

/// The server's signing key.
#[derive(Clone)]
pub struct Key {
    pub seed: [u8; 32],
    pub public: [u8; 32],
}

impl Federation {
    pub fn new(db: Db, data_dir: PathBuf) -> Federation {
        Federation {
            db,
            data_dir,
            key_file: Mutex::new(None),
            key: OnceLock::new(),
            allow_private: AtomicBool::new(false),
            public_url: Mutex::new(None),
            clients: OnceLock::new(),
            discovery: Mutex::new(HashMap::new()),
            jwks: Mutex::new(HashMap::new()),
            issued: Mutex::new(HashMap::new()),
            obtained: Mutex::new(HashMap::new()),
            opened: Mutex::new(HashMap::new()),
            live_mark: Default::default(),
            healths: Mutex::new(HashMap::new()),
            keys_asked: Mutex::new(HashMap::new()),
        }
    }

    pub fn set_key_file(&self, file: Option<PathBuf>) {
        *self.key_file.lock().expect("not poisoned") = file;
    }

    pub fn set_allow_private(&self, allow: bool) {
        self.allow_private.store(allow, Ordering::Relaxed);
    }

    pub fn allow_private(&self) -> bool {
        self.allow_private.load(Ordering::Relaxed)
    }

    pub fn set_public_url(&self, url: Option<String>) {
        *self.public_url.lock().expect("not poisoned") =
            url.map(|u| u.trim_end_matches('/').to_string());
    }

    /// `PUBLIC_URL`, without a trailing slash.
    pub fn base(&self) -> Option<String> {
        self.public_url.lock().expect("not poisoned").clone()
    }

    /// Our host as OCM names it (`host[:port]`), from `PUBLIC_URL`.
    pub fn host(&self) -> Option<String> {
        let base = self.base()?;
        let url = reqwest::Url::parse(&base).ok()?;
        let name = url.host_str()?;
        host_of(&match url.port() {
            Some(port) => format!("{name}:{port}"),
            None => name.to_string(),
        })
    }

    /// The signing key: read, or made at the first start in the data dir
    /// (`<DATA_DIR>/ocm-key`, 0600).
    pub fn key(&self) -> std::io::Result<Key> {
        if let Some(key) = self.key.get() {
            return Ok(key.clone());
        }
        let named = self.key_file.lock().expect("not poisoned").clone();
        let seed = match &named {
            Some(file) => read_seed(file)?,
            None => {
                let file = self.data_dir.join("ocm-key");
                match read_seed(&file) {
                    Ok(seed) => seed,
                    Err(e) if e.kind() == std::io::ErrorKind::NotFound => make_seed(&file)?,
                    Err(e) => return Err(e),
                }
            }
        };
        let public = ed25519_dalek::SigningKey::from_bytes(&seed)
            .verifying_key()
            .to_bytes();
        Ok(self.key.get_or_init(|| Key { seed, public }).clone())
    }

    /// The key's id in our JWK Set: our host and a short fingerprint, so a
    /// new key has a new id.
    pub fn kid(&self) -> std::io::Result<String> {
        let key = self.key()?;
        let host = self.host().unwrap_or_default();
        let print = crate::util::sha256_hex(&hex::encode(key.public));
        Ok(format!("{host}#{}", &print[..16]))
    }

    /// Our JWK Set (§6, Appendix B.1).
    pub fn jwks(&self) -> std::io::Result<serde_json::Value> {
        let key = self.key()?;
        let jwk = Jwk {
            kty: "OKP".into(),
            crv: Some("Ed25519".into()),
            alg: Some("Ed25519".into()),
            kid: Some(self.kid()?),
            x: Some(crate::util::base64url_encode(&key.public)),
            use_: Some("sig".into()),
        };
        Ok(serde_json::json!({ "keys": [jwk] }))
    }

    /// The stored settings.
    async fn stored_settings(&self) -> sqlx::Result<Settings> {
        crate::server_settings::load(self.db.reader(), "federation").await
    }

    /// The superadmin's choice.
    pub async fn mode(&self) -> sqlx::Result<Mode> {
        if self.host().is_none() {
            return Ok(Mode::Off);
        }
        Ok(self.stored_settings().await?.mode)
    }

    /// The mode as stored, whatever `PUBLIC_URL` says.
    pub async fn stored_mode(&self) -> sqlx::Result<Mode> {
        Ok(self.stored_settings().await?.mode)
    }

    /// Whether federation is on.
    pub async fn on(&self) -> sqlx::Result<bool> {
        Ok(self.mode().await? != Mode::Off)
    }

    /// What we make of a server: refused, or allowed (listed or not).
    /// Ourselves never.
    pub async fn trust(&self, host: &str) -> sqlx::Result<Trust> {
        let host = host.to_lowercase();
        if self.host().as_deref() == Some(host.as_str()) {
            return Ok(Trust::Refused);
        }
        let list: Option<String> =
            sqlx::query_scalar("SELECT list FROM federation_servers WHERE host = ?1")
                .bind(&host)
                .fetch_optional(self.db.reader())
                .await?;
        Ok(match (self.mode().await?, list.as_deref()) {
            (Mode::Off, _) => Trust::Refused,
            (Mode::Listed, Some("allowed")) => Trust::Allowed { listed: true },
            (Mode::Listed, _) => Trust::Refused,
            (Mode::Open, Some("refused")) => Trust::Refused,
            (Mode::Open, _) => Trust::Allowed { listed: false },
        })
    }

    /// Our discovery (§5.3), or `enabled: false` while federation is off.
    pub async fn discovery(&self) -> sqlx::Result<serde_json::Value> {
        let mode = self.mode().await?;
        let Some(base) = self.base().filter(|_| mode != Mode::Off) else {
            return Ok(serde_json::json!({
                "enabled": false,
                "apiVersion": API_VERSION,
                "endPoint": format!("{}{OCM_PREFIX}", self.base().unwrap_or_default()),
                "resourceTypes": [],
            }));
        };
        let protocols = serde_json::json!({
            "webdav": format!("{DAV_PREFIX}/"),
            "webdav-receive": { "uri": "relative" },
        });
        let resource = |name: &str| {
            serde_json::json!({
                "name": name,
                "shareTypes": ["user"],
                "protocols": protocols,
            })
        };
        let list = match mode {
            Mode::Listed => "allowlist",
            _ => "denylist",
        };
        Ok(serde_json::json!({
            "enabled": true,
            "apiVersion": API_VERSION,
            "endPoint": format!("{base}{OCM_PREFIX}"),
            "provider": "Organizer",
            "resourceTypes": [resource("file"), resource("folder")],
            "capabilities": [
                "exchange-token", "http-sig", "invites", "notifications",
                DRIVES, E2E, LIVE,
            ],
            "criteria": ["must-use-http-sig", "must-exchange-token", list],
            "inviteAcceptDialog": "/settings/contacts",
            "jwksUri": format!("{base}{OCM_PREFIX}/jwks"),
            "tokenEndPoint": format!("{base}{OCM_PREFIX}/token"),
        }))
    }

    /// Our client to other servers: no redirects, public addresses only
    /// unless `FEDERATION_ALLOW_PRIVATE`.
    pub fn client(&self) -> reqwest::Client {
        let (public, private) = self.clients.get_or_init(|| {
            let make = |allow_private: bool| {
                let mut builder = reqwest::Client::builder()
                    .timeout(TIMEOUT)
                    .user_agent(concat!("Organizer/", env!("CARGO_PKG_VERSION")))
                    .redirect(reqwest::redirect::Policy::none());
                if !allow_private {
                    builder = builder.dns_resolver(Arc::new(crate::drop_box::link::PublicOnly));
                }
                builder.build().expect("an HTTP client")
            };
            (make(false), make(true))
        });
        if self.allow_private() {
            private.clone()
        } else {
            public.clone()
        }
    }

    /// Whether a URL of another server may be asked: https at a public
    /// address, or http and private ones with `FEDERATION_ALLOW_PRIVATE`
    /// (the testing setups OCM allows).
    pub fn allowed_url(&self, url: &str) -> Result<reqwest::Url, String> {
        let url = reqwest::Url::parse(url).map_err(|_| "not a URL".to_string())?;
        let private = self.allow_private();
        if url.scheme() != "https" && !(private && url.scheme() == "http") {
            return Err("other servers are reached over https".into());
        }
        if !crate::drop_box::link::allowed(&url, private) {
            return Err("that server is at an address that is not allowed".into());
        }
        Ok(url)
    }

    /// The bases a server's host is tried at: https, then (with
    /// `FEDERATION_ALLOW_PRIVATE`) http.
    fn bases(&self, host: &str) -> Vec<String> {
        let mut bases = vec![format!("https://{host}")];
        if self.allow_private() {
            bases.push(format!("http://{host}"));
        }
        bases
    }

    /// GET a JSON document of another server, at most [`MAX_ANSWER`].
    pub async fn get_json<T: serde::de::DeserializeOwned>(
        &self,
        url: &str,
        accept: &str,
    ) -> Result<T, String> {
        let url = self.allowed_url(url)?;
        let response = self
            .client()
            .get(url)
            .header("accept", accept)
            .send()
            .await
            .map_err(|_| "the server cannot be reached".to_string())?;
        if !response.status().is_success() {
            return Err(format!(
                "the server answered {}",
                response.status().as_u16()
            ));
        }
        let bytes = read_capped(response).await?;
        serde_json::from_slice(&bytes).map_err(|_| "the server's answer is not valid".into())
    }

    /// Another server's discovery: kept a few minutes, a failure 30
    /// seconds. Its `endPoint` must be at the host it was asked of.
    pub async fn discover(&self, host: &str) -> Result<Arc<Discovery>, String> {
        let host = host_of(host).ok_or("not a server's address")?;
        if let Some((at, found)) = self.discovery.lock().expect("not poisoned").get(&host) {
            let keep = if found.is_ok() {
                KEEP_DISCOVERY
            } else {
                KEEP_FAILURE
            };
            if at.elapsed() < keep {
                return found.clone();
            }
        }
        let mut found = Err("the server has no OCM discovery".to_string());
        for base in self.bases(&host) {
            match self
                .get_json::<Discovery>(&format!("{base}/.well-known/ocm"), "application/json")
                .await
            {
                Ok(d) if d.enabled => {
                    self.note_server(&host, &d);
                    found = Ok(Arc::new(d));
                    break;
                }
                Ok(_) => {
                    found = Err("the server does not take part in OCM".into());
                    break;
                }
                Err(e) => found = Err(e),
            }
        }
        self.discovery
            .lock()
            .expect("not poisoned")
            .insert(host, (Instant::now(), found.clone()));
        found
    }

    /// What a server's discovery says it is, onto the contacts there
    /// (`Contact.provider`, `organizer`). In a task of its own: a caller
    /// may hold the one writer connection.
    fn note_server(&self, host: &str, discovery: &Discovery) {
        let db = self.db.clone();
        let host = host.to_string();
        let (provider, organizer) = (discovery.provider_name(), discovery.organizer());
        tokio::spawn(async move {
            let noted = sqlx::query(
                "UPDATE contacts SET provider = ?1, organizer = ?2
                 WHERE host = ?3 AND (provider IS NOT ?1 OR organizer IS NOT ?2)",
            )
            .bind(provider)
            .bind(organizer)
            .bind(&host)
            .execute(db.writer())
            .await;
            if let Err(e) = noted {
                tracing::warn!("federation: noting what {host} is: {e}");
            }
        });
    }

    /// Forget what is kept of a server (its discovery and keys): asked
    /// anew next time.
    pub fn forget(&self, host: &str) {
        self.discovery.lock().expect("not poisoned").remove(host);
    }

    /// A server's public key by its id, from its JWK Set (fetched again
    /// once when the id is not there: a new key).
    async fn key_of(&self, discovery: &Discovery, keyid: &str) -> Result<[u8; 32], String> {
        let uri = discovery
            .jwks_uri
            .clone()
            .ok_or("the server advertises no jwksUri")?;
        for again in [false, true] {
            let kept = self
                .jwks
                .lock()
                .expect("not poisoned")
                .get(&uri)
                .filter(|(at, _)| at.elapsed() < KEEP_DISCOVERY && !again)
                .map(|(_, keys)| keys.clone());
            let keys = match kept {
                Some(keys) => keys,
                None => {
                    #[derive(Deserialize)]
                    struct Set {
                        keys: Vec<Jwk>,
                    }
                    let set: Set = self
                        .get_json(&uri, "application/jwk-set+json, application/json")
                        .await?;
                    let keys = Arc::new(set.keys);
                    self.jwks
                        .lock()
                        .expect("not poisoned")
                        .insert(uri.clone(), (Instant::now(), keys.clone()));
                    keys
                }
            };
            if let Some(jwk) = keys.iter().find(|k| k.kid.as_deref() == Some(keyid)) {
                return jwk
                    .ed25519()
                    .ok_or_else(|| "the signing key's alg is not one we take".to_string());
            }
        }
        Err("the server's JWK Set has no key of that keyid".into())
    }

    /// Check a request another server signed (§6.4), as coming from
    /// `host`: one signature tagged `ocm`, its components and freshness,
    /// the body's digest, and the signature with the key of that keyid in
    /// the host's JWK Set. We advertise `must-use-http-sig`: an unsigned
    /// request is refused.
    pub async fn check_signed(
        &self,
        parts: &axum::http::request::Parts,
        body: &[u8],
        host: &str,
    ) -> Result<(), String> {
        let signature = sign::ocm_signature(&parts.headers)?
            .ok_or("the request is not signed (this server requires HTTP signatures)")?;
        let base = self.base().ok_or("federation is off")?;
        let target_uri = format!(
            "{base}{}",
            parts
                .uri
                .path_and_query()
                .map(|p| p.as_str())
                .unwrap_or("/")
        );
        let received = sign::Received {
            method: parts.method.as_str(),
            target_uri: &target_uri,
            headers: &parts.headers,
            body,
        };
        let keyid = sign::check_shape(&signature, &received)?;
        let discovery = self.discover(host).await?;
        if !discovery.has("http-sig") && discovery.jwks_uri.is_none() {
            return Err("the server publishes no keys".into());
        }
        let key = self.key_of(&discovery, &keyid).await?;
        sign::verify(&signature, &received, &key)
    }

    /// POST to another server, signed: the status and the body (at most
    /// [`MAX_ANSWER`]).
    pub async fn post_signed(
        &self,
        url: &str,
        content_type: &str,
        body: Vec<u8>,
    ) -> Result<(u16, Vec<u8>), String> {
        let parsed = self.allowed_url(url)?;
        let key = self.key().map_err(|_| "the signing key cannot be read")?;
        let kid = self.kid().map_err(|_| "the signing key cannot be read")?;
        let headers = sign::sign(&key.seed, &kid, "POST", parsed.as_str(), &body)?;
        let mut request = self
            .client()
            .post(parsed)
            .header("content-type", content_type)
            .header("accept", "application/json");
        for (name, value) in headers {
            request = request.header(name, value);
        }
        let response = request
            .body(body)
            .send()
            .await
            .map_err(|_| "the server cannot be reached".to_string())?;
        let status = response.status().as_u16();
        Ok((status, read_capped(response).await?))
    }

    /// POST JSON to another server's OCM API (`endPoint` + path), signed.
    pub async fn post_json(
        &self,
        discovery: &Discovery,
        path: &str,
        body: &serde_json::Value,
    ) -> Result<(u16, Vec<u8>), String> {
        let url = format!("{}{path}", discovery.end_point.trim_end_matches('/'));
        self.post_signed(&url, "application/json", body.to_string().into_bytes())
            .await
    }

    /// A new token of our code flow for a share (§10.2).
    pub fn issue(&self, share: Uuid) -> String {
        let token = format!("ocmt_{}", crate::util::random_hex(32));
        let mut issued = self.issued.lock().expect("not poisoned");
        issued.retain(|_, (_, until)| *until > Instant::now());
        issued.insert(
            crate::util::sha256_hex(&token),
            (share, Instant::now() + Duration::from_secs(TOKEN_SECS)),
        );
        token
    }

    /// The share a token of our code flow opens, while it lives.
    pub fn redeem(&self, token: &str) -> Option<Uuid> {
        let issued = self.issued.lock().expect("not poisoned");
        issued
            .get(&crate::util::sha256_hex(token))
            .filter(|(_, until)| *until > Instant::now())
            .map(|(share, _)| *share)
    }

    /// Drop every token of a share (unshared).
    pub fn revoke_tokens(&self, share: Uuid) {
        self.issued
            .lock()
            .expect("not poisoned")
            .retain(|_, (s, _)| *s != share);
    }

    pub(crate) fn token_for(&self, received: Uuid) -> Option<String> {
        self.obtained
            .lock()
            .expect("not poisoned")
            .get(&received)
            .filter(|(_, until)| *until > Instant::now())
            .map(|(token, _)| token.clone())
    }

    pub(crate) fn keep_token(&self, received: Uuid, token: String, secs: u64) {
        // Renewed a little before it ends (§10.2: no refresh token).
        let life = Duration::from_secs(secs.saturating_sub(20).max(1));
        self.obtained
            .lock()
            .expect("not poisoned")
            .insert(received, (token, Instant::now() + life));
    }

    /// Whether a share from another server answers (by its drive).
    pub fn health(&self, drive: Uuid) -> Arc<crate::storage::Health> {
        self.healths
            .lock()
            .expect("not poisoned")
            .entry(drive)
            .or_default()
            .clone()
    }

    /// Note that someone opened a share from another server: it is
    /// followed sooner for a while.
    pub fn opened(&self, drive: Uuid) {
        self.opened
            .lock()
            .expect("not poisoned")
            .insert(drive, Instant::now());
    }

    pub(crate) fn drop_token(&self, received: Uuid) {
        self.obtained
            .lock()
            .expect("not poisoned")
            .remove(&received);
    }
}

/// A response's body, at most [`MAX_ANSWER`].
async fn read_capped(mut response: reqwest::Response) -> Result<Vec<u8>, String> {
    let mut bytes = Vec::new();
    while let Some(chunk) = response
        .chunk()
        .await
        .map_err(|_| "the server's answer broke off".to_string())?
    {
        if bytes.len() + chunk.len() > MAX_ANSWER {
            return Err("the server's answer is too large".into());
        }
        bytes.extend_from_slice(&chunk);
    }
    Ok(bytes)
}

fn read_seed(file: &std::path::Path) -> std::io::Result<[u8; 32]> {
    let bytes = std::fs::read(file)?;
    bytes.as_slice().try_into().map_err(|_| {
        std::io::Error::other(format!(
            "{} must hold the 32 bytes of an Ed25519 key's seed",
            file.display()
        ))
    })
}

/// A new signing key, written where only this user reads it.
fn make_seed(file: &std::path::Path) -> std::io::Result<[u8; 32]> {
    let mut seed = [0u8; 32];
    rand::Rng::fill_bytes(&mut rand::rng(), &mut seed);
    if let Some(dir) = file.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    std::os::unix::fs::OpenOptionsExt::mode(&mut options, 0o600);
    std::io::Write::write_all(&mut options.open(file)?, &seed)?;
    tracing::info!("made the federation signing key {}", file.display());
    Ok(seed)
}

/// A user's OCM address on this server: their email at our host.
pub fn address_of(email: &str, host: &str) -> String {
    format!("{email}@{host}")
}

/// OCM's WebDAV permissions for a role (§4): viewer → read; editor →
/// read, write; manager → read, write, share.
pub fn permissions_of(role: crate::files::roles::Role) -> Vec<&'static str> {
    use crate::files::roles::Role;
    match role {
        Role::Viewer | Role::UploadOnly => vec!["read"],
        Role::Editor => vec!["read", "write"],
        Role::Manager | Role::Owner => vec!["read", "write", "share"],
    }
}

/// The role OCM's permissions stand for, for `my_role` (§5).
pub fn role_of(permissions: &[String]) -> crate::files::roles::Role {
    use crate::files::roles::Role;
    let has = |p: &str| permissions.iter().any(|x| x == p);
    if has("write") && has("share") {
        Role::Manager
    } else if has("write") {
        Role::Editor
    } else {
        Role::Viewer
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::files::roles::Role;

    #[test]
    fn roles_map_to_permissions_and_back() {
        for role in [Role::Viewer, Role::Editor, Role::Manager] {
            let perms: Vec<String> = permissions_of(role).iter().map(|p| p.to_string()).collect();
            assert_eq!(role_of(&perms), role);
        }
        assert_eq!(role_of(&["write".into()]), Role::Editor);
        assert_eq!(role_of(&[]), Role::Viewer);
    }

    #[tokio::test]
    async fn the_key_is_made_once_and_published() {
        let db = crate::db::open(":memory:").await.unwrap();
        let dir = std::env::temp_dir().join(format!("organizer-ocm-{}", Uuid::now_v7()));
        let federation = Federation::new(db.clone(), dir.clone());
        federation.set_public_url(Some("https://cloud.example.org/".into()));
        assert_eq!(federation.host().as_deref(), Some("cloud.example.org"));
        let key = federation.key().unwrap();
        let again = Federation::new(db, dir.clone());
        assert_eq!(again.key().unwrap().seed, key.seed, "read back");
        let jwks = federation.jwks().unwrap();
        let jwk: Jwk = serde_json::from_value(jwks["keys"][0].clone()).unwrap();
        assert_eq!(jwk.ed25519(), Some(key.public));
        assert!(jwk.kid.unwrap().starts_with("cloud.example.org#"));
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(dir.join("ocm-key"))
                .unwrap()
                .permissions()
                .mode();
            assert_eq!(mode & 0o777, 0o600);
        }
        let _ = std::fs::remove_dir_all(dir);
    }
}

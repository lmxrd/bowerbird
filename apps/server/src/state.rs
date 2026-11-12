use crate::ai::Ai;
use crate::db::Db;
use crate::files::Files;
use crate::jobs::Jobs;
use crate::limits::Limits;
use crate::oidc::Oidc;
use crate::vectors::VectorStore;
use std::sync::Arc;

#[derive(Clone)]
pub struct AppState {
    pub db: Db,
    /// The users' trees: the index, the reconciler and the operations.
    pub files: Arc<Files>,
    /// `PUBLIC_URL`, without a trailing slash: where Organizer is reached from.
    pub public_url: Option<String>,
    /// `PUBLIC_URL` is https: every cookie is `Secure` and `__Host-`.
    pub secure_cookies: bool,
    /// `TRUST_PROXY`: forwarded headers (`X-Forwarded-For`, ...) count.
    pub trust_proxy: bool,
    /// `SWAGGER_UI`: serve the OpenAPI spec and Swagger UI.
    pub swagger_ui: bool,
    /// How often sign-ins and codes may be tried (`limits.rs`).
    pub limits: Arc<Limits>,
    /// Signs the cookie of a sign-in's two-factor step. Made at start: a
    /// restart only means signing in again from the password.
    pub challenge_key: axum_extra::extract::cookie::Key,
    /// Single sign-on: the stored settings with the OIDC_* variables on top.
    pub oidc: Arc<Oidc>,
    /// `TOTP_REQUIRED`: every user must have two-factor authentication on;
    /// one without it sets it up at their next sign-in.
    pub totp_required: bool,
    /// The connection to the AI model: the stored settings with the AI_*
    /// variables on top.
    pub ai: Arc<Ai>,
    /// The background jobs.
    pub jobs: Arc<Jobs>,
    /// `LINKS_ALLOW_PRIVATE`: links of the drop box may be fetched from
    /// private and local addresses (`drop_box/link.rs`).
    pub links_allow_private: bool,
    /// Where the embeddings are kept (`VECTOR_DATABASE_URL`).
    pub vectors: Arc<dyn VectorStore>,
    /// The backup settings: the stored ones with the BACKUP_* variables on
    /// top.
    pub backup: Arc<crate::backup::Backup>,
    /// `SIGNUP_OPEN` and `OIDC_CREATE_USERS`, over the stored switches of
    /// how people join (`users.rs`).
    pub joining: crate::users::Env,
    /// Push notifications: the VAPID key and APNs (`push.rs`).
    pub push: Arc<crate::push::Push>,
}

impl AppState {
    /// The state with every setting off and no single sign-on, around a
    /// database and a data dir: what the tests start from.
    pub async fn new(db: Db, data_dir: std::path::PathBuf) -> Self {
        let oidc = Arc::new(Oidc::load(&db, crate::oidc::Env::default()).await);
        let ai = Arc::new(Ai::load(&db, crate::ai::Env::default()).await);
        let jobs = Jobs::new(db.clone(), Vec::new());
        let backup = Arc::new(crate::backup::Backup::load(&db, Default::default()).await);
        let vectors = crate::vectors::open("turso::memory:")
            .await
            .expect("an in-memory vector store");
        let push = Arc::new(
            crate::push::Push::load(&db, Default::default(), false)
                .await
                .expect("the push keys"),
        );
        Self {
            push,
            files: Files::new(
                db.clone(),
                data_dir.clone(),
                crate::config::files_dir(&data_dir).expect("a valid FILES_DIR in the tests"),
            ),
            db,
            public_url: None,
            secure_cookies: false,
            trust_proxy: false,
            swagger_ui: false,
            limits: Arc::new(Limits::default()),
            challenge_key: axum_extra::extract::cookie::Key::generate(),
            oidc,
            totp_required: false,
            ai,
            jobs,
            links_allow_private: false,
            vectors,
            backup,
            joining: Default::default(),
        }
    }
}

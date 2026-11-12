use anyhow::Context;
use organizer::{app, config, db, state::AppState};
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    // The container healthcheck: no database, no server, only a request to
    // the one already running (the same command as Potoo's and Woodmouse's).
    // Its error ends the process with status 1.
    if std::env::args().nth(1).as_deref() == Some("healthcheck") {
        return healthcheck().await.context("unhealthy");
    }
    // The backups, from the command line (`backup/cli.rs`).
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.first().map(String::as_str) == Some("backup") {
        organizer::install_tls_provider();
        return organizer::backup::cli::main(&args[1..]).await;
    }
    // Recovery: the superadmin role and the second factors (`recovery.rs`).
    if matches!(
        args.first().map(String::as_str),
        Some("superadmin" | "two-factor")
    ) {
        return organizer::recovery::main(&args).await;
    }

    // RUST_LOG picks what is logged (default: info).
    tracing_subscriber::fmt()
        .with_env_filter(
            // rustic tells every step of a backup at info.
            tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| {
                "info,rustic_core=warn,rustic_backend=warn,opendal=warn".into()
            }),
        )
        .init();

    // The binary is built for musl (the image is Alpine), so the TLS stack
    // is ring rather than aws-lc; reqwest is built without a provider and
    // uses this default one.
    organizer::install_tls_provider();

    let port = config::port().map_err(anyhow::Error::msg)?;
    let data_dir = config::data_dir();
    std::fs::create_dir_all(&data_dir).with_context(|| format!("cannot create {data_dir:?}"))?;
    let db_path = config::database_path(&data_dir).map_err(anyhow::Error::msg)?;
    let db = db::open(&db_path).await?;
    let vectors = organizer::vectors::open(&config::vector_database_url(&data_dir)).await?;
    let public_url = config::public_url();
    let limits = Arc::new(organizer::limits::Limits::default());
    // The users' trees: the journal is settled, then each tree is indexed
    // and watched in the background. `STORAGE_URL` may be an `s3://bucket/prefix`
    // URL instead of a path: the file content is then in that bucket.
    let storage_url = config::storage_url(&data_dir).map_err(anyhow::Error::msg)?;
    let files = organizer::files::Files::with_background(db.clone(), data_dir.clone(), storage_url);
    files.set_retention_env(organizer::retention::Partial::from_env().map_err(anyhow::Error::msg)?);
    // The storage backends (docs/M7C_DESIGN.md): the server key over their
    // secrets (`SECRET_KEY_FILE`, else made in the data dir at the first
    // start), and whether users' own may be at private addresses.
    files
        .backends()
        .set_key_file(config::var("SECRET_KEY_FILE").map(std::path::PathBuf::from));
    // An `s3://` STORAGE_URL: the bucket is the default backend, and new
    // drives are made in it (the `users_my_drive` trigger reads this).
    if let organizer::config::StorageUrl::S3 {
        endpoint,
        region,
        bucket,
        prefix,
    } = files.storage_url()
    {
        let backend = files
            .backends()
            .ensure_files_backend(endpoint, region, bucket, prefix)
            .await
            .map_err(anyhow::Error::msg)?;
        organizer::server_settings::save(db.writer(), "files_backend", &backend.to_string())
            .await?;
    }
    files
        .backends()
        .set_allow_private(env_flag("BACKENDS_ALLOW_PRIVATE")?);
    // Federation (docs/M7E_DESIGN.md): the signing key (`OCM_KEY_FILE`,
    // which must exist, else made in the data dir at the first start),
    // whether other servers may be at private addresses, and our address.
    let federation = files.federation();
    federation.set_key_file(config::var("OCM_KEY_FILE").map(std::path::PathBuf::from));
    federation.set_allow_private(env_flag("FEDERATION_ALLOW_PRIVATE")?);
    federation.set_public_url(public_url.clone());
    federation
        .key()
        .context("cannot read or make the federation signing key")?;
    files.start().await?;
    // Push notifications: the VAPID key (made at first start) and APNs.
    let links_allow_private = env_flag("LINKS_ALLOW_PRIVATE")?;
    let push = Arc::new(
        organizer::push::Push::load(
            &db,
            organizer::push::Env::from_env().map_err(anyhow::Error::msg)?,
            links_allow_private,
        )
        .await?,
    );
    let state = AppState {
        push,
        files,
        db: db.clone(),
        secure_cookies: public_url
            .as_deref()
            .is_some_and(|u| u.starts_with("https://")),
        public_url,
        trust_proxy: env_flag("TRUST_PROXY")?,
        swagger_ui: env_flag("SWAGGER_UI")?,
        limits: limits.clone(),
        challenge_key: axum_extra::extract::cookie::Key::generate(),
        // Single sign-on: the stored settings, with the OIDC_* environment
        // variables on top.
        oidc: Arc::new(
            organizer::oidc::Oidc::load(
                &db,
                organizer::oidc::Env::from_env().map_err(anyhow::Error::msg)?,
            )
            .await,
        ),
        totp_required: env_flag("TOTP_REQUIRED")?,
        // The AI connection: the stored settings, with the AI_* environment
        // variables on top.
        ai: Arc::new(
            organizer::ai::Ai::load(
                &db,
                organizer::ai::Env::from_env().map_err(anyhow::Error::msg)?,
            )
            .await,
        ),
        jobs: organizer::jobs::Jobs::new(db.clone(), organizer::handlers()),
        links_allow_private,
        vectors,
        // The backups: the stored settings, with the BACKUP_* variables on
        // top.
        backup: Arc::new(
            organizer::backup::Backup::load(
                &db,
                organizer::backup::Env::from_env().map_err(anyhow::Error::msg)?,
            )
            .await,
        ),
        // How people join: SIGNUP_OPEN and OIDC_CREATE_USERS over the stored
        // switches.
        joining: organizer::users::Env::from_env().map_err(anyhow::Error::msg)?,
    };
    state.jobs.start(state.clone()).await?;
    // The search index follows the trees.
    tokio::spawn(organizer::search::run(state.clone()));
    // The trash and the versions are kept as the users' rules say.
    tokio::spawn(organizer::retention::run(state.clone()));
    // Backups are queued when they are due.
    tokio::spawn(organizer::backup::schedule(state.clone()));
    // A notice when items of the drop box wait for review.
    tokio::spawn(organizer::push::run(state.clone()));
    // Federation: the shares from other servers are followed, and the
    // servers of ours told of changes (`organizer-live`).
    tokio::spawn(organizer::federation::incoming::run_follow(
        state.files.clone(),
    ));
    tokio::spawn(organizer::federation::outgoing::run_live(
        state.files.clone(),
    ));
    let swagger_ui = state.swagger_ui;

    // With OIDC_ONLY and no provider, nobody can sign in (but the first
    // superadmin on /setup, who can then set the provider up): say so.
    if !state.oidc.passwords_allowed() && !state.oidc.settings().enabled() {
        tracing::warn!(
            "OIDC_ONLY is set but single sign-on is not set up: nobody can sign in \
             until it is (set the OIDC_* variables, or set it up in the superadmin settings)"
        );
    }

    // The sign-in limits forget the keys that are back to their full quota.
    tokio::spawn(async move {
        loop {
            tokio::time::sleep(Duration::from_secs(600)).await;
            limits.forget_idle();
        }
    });

    // Expired sessions are deleted every hour (a session past its expiry
    // is refused anyway: this only keeps the table small).
    {
        use tower_sessions::ExpiredDeletion;
        let store = organizer::session::Store::new(db.clone());
        tokio::spawn(async move {
            loop {
                if let Err(e) = store.delete_expired().await {
                    tracing::warn!("cannot delete the expired sessions: {e}");
                }
                tokio::time::sleep(Duration::from_secs(3_600)).await;
            }
        });
    }

    let listener = tokio::net::TcpListener::bind(("0.0.0.0", port))
        .await
        .with_context(|| format!("cannot listen on {port}"))?;
    tracing::info!("organizer listening on http://0.0.0.0:{port}");
    if swagger_ui {
        tracing::info!(
            "the API is described at {} (Swagger UI at {})",
            organizer::SPEC_PATH,
            organizer::SWAGGER_UI_PATH
        );
    }
    // On SIGINT or SIGTERM the server stops accepting and finishes the
    // requests in flight; closing the pools then checkpoints the WAL, so the
    // database file alone is complete. The client's address (for the
    // sign-in limits) comes from the connection, unless TRUST_PROXY says to
    // read the forwarded one.
    axum::serve(
        listener,
        app(state).into_make_service_with_connect_info::<SocketAddr>(),
    )
    .with_graceful_shutdown(stop_signal())
    .await
    .context("the server failed")?;
    db.close().await;
    Ok(())
}

/// A yes or no environment variable: unset or empty is no, and a value
/// that is not understood stops the start rather than quietly meaning no.
fn env_flag(name: &str) -> anyhow::Result<bool> {
    Ok(config::flag(name)
        .map_err(anyhow::Error::msg)?
        .unwrap_or(false))
}

/// Resolves on SIGINT or SIGTERM.
async fn stop_signal() {
    use tokio::signal::unix::{SignalKind, signal};
    let mut term = signal(SignalKind::terminate()).expect("cannot listen for SIGTERM");
    tokio::select! {
        _ = tokio::signal::ctrl_c() => {}
        _ = term.recv() => {}
    }
}

/// `GET /api/health` on this machine, by hand, so that the healthcheck needs
/// nothing from the image (no wget or curl). Succeeds on a 200, fails
/// otherwise.
async fn healthcheck() -> anyhow::Result<()> {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    let port = config::port().map_err(anyhow::Error::msg)?;
    let mut stream = tokio::net::TcpStream::connect(("127.0.0.1", port)).await?;
    stream
        .write_all(b"GET /api/health HTTP/1.0\r\nHost: localhost\r\n\r\n")
        .await?;
    let mut head = [0u8; 12];
    stream.read_exact(&mut head).await?;
    anyhow::ensure!(
        &head == b"HTTP/1.1 200" || &head == b"HTTP/1.0 200",
        "the health endpoint did not answer 200"
    );
    Ok(())
}

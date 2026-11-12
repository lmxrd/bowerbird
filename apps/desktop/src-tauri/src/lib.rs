//! Organizer's desktop app (docs/M5_DESIGN.md §4): the sync engine
//! (`organizer-sync`) in the background, a tray (menu bar) menu, and a
//! window with the sync status, the drop box's review and the settings.
//!
//! The window is a small React app (`apps/desktop/src`) built from the
//! web app's components. It talks to the server through the `api` command,
//! which sends its requests with the device's token: the token stays in
//! Rust and in the system's credential store, never in the web view, and
//! the server needs no CORS for the app.

mod commands;
#[cfg(target_os = "macos")]
mod file_provider;
mod settings;
mod system;
mod tray;

use organizer_sync::{Config, Engine, Event, Role, Status, Waiting};
use serde::Serialize;
use settings::Settings;
use std::path::PathBuf;
use std::sync::Mutex;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;
use tauri::{AppHandle, Emitter, Manager, WindowEvent};
use tauri_plugin_autostart::ManagerExt as _;
use tauri_plugin_notification::NotificationExt as _;

/// The app's state, managed by Tauri.
pub struct App {
    settings: Mutex<Settings>,
    settings_path: PathBuf,
    data_dir: PathBuf,
    home: PathBuf,
    engine: tokio::sync::Mutex<Option<Engine>>,
    /// The drop box's items that wait for the user.
    waiting: AtomicUsize,
    tray: Mutex<Option<tray::Items>>,
}

/// What the window shows: one value, sent again whenever it changes
/// (the `state` event).
#[derive(Clone, Serialize)]
pub struct View {
    signed_in: bool,
    server: Option<String>,
    email: Option<String>,
    status: Status,
    paused: bool,
    folders: Vec<FolderView>,
    drop_box: bool,
    drop_box_path: String,
    organizer_dir: String,
    autostart: bool,
    ignore: Vec<String>,
    waiting: usize,
    device_name: String,
    /// How this system keeps on-demand folders (`file_provider` on macOS,
    /// `cloud_files` on Windows); None: full sync only (Linux).
    on_demand: Option<&'static str>,
}

/// How this system keeps on-demand folders (docs/M5B_DESIGN.md).
pub fn on_demand() -> Option<&'static str> {
    if cfg!(target_os = "macos") {
        Some("file_provider")
    } else if cfg!(windows) {
        Some("cloud_files")
    } else {
        None
    }
}

#[derive(Clone, Serialize)]
pub struct FolderView {
    id: i64,
    name: String,
    local_path: String,
    mode: String,
    /// The folder of the server synced (a drive's root, or a folder in it).
    remote_id: String,
    /// The drive it is in (docs/M7A_DESIGN.md); None until the engine has
    /// asked the server.
    drive_id: Option<String>,
    /// No longer on the server, or its drive left or deleted: not synced,
    /// its files kept here.
    unavailable: bool,
    /// Its drive's storage is unavailable, or the drive is being moved
    /// (docs/M7C_DESIGN.md): not synced until it is back, its changes
    /// kept here. Why, as the last pass found it.
    waiting: Option<Waiting>,
    /// What the user may do with it (docs/M7B_DESIGN.md §1), as the engine
    /// last listed it; None before.
    role: Option<Role>,
}

impl App {
    /// `~/Organizer`, where the synced folders go by default.
    pub fn organizer_dir(&self) -> PathBuf {
        self.home.join("Organizer")
    }

    pub fn drop_box_dir(&self) -> PathBuf {
        self.organizer_dir().join("Drop box")
    }

    pub fn settings(&self) -> Settings {
        self.settings.lock().unwrap().clone()
    }

    pub fn update_settings(&self, f: impl FnOnce(&mut Settings)) {
        let mut settings = self.settings.lock().unwrap();
        f(&mut settings);
        if let Err(e) = settings.save(&self.settings_path) {
            tracing::warn!("the settings could not be saved: {e}");
        }
    }

    pub async fn engine(&self) -> Result<Engine, String> {
        self.engine
            .lock()
            .await
            .clone()
            .ok_or_else(|| "Not signed in".to_string())
    }

    pub async fn view(&self, app: &AppHandle) -> View {
        let settings = self.settings();
        let engine = self.engine.lock().await.clone();
        // The status is read (and its lock let go) before anything awaits.
        let (status, paused) = match &engine {
            Some(engine) => (engine.status().borrow().clone(), engine.is_paused()),
            None => (Status::default(), false),
        };
        let mut folders = Vec::new();
        if let Some(engine) = &engine {
            for f in engine.folders().await.unwrap_or_default() {
                let role = engine.role(&f).await.ok().flatten();
                folders.push(FolderView {
                    id: f.id,
                    name: f
                        .local()
                        .file_name()
                        .map(|n| n.to_string_lossy().into_owned())
                        .unwrap_or_default(),
                    unavailable: status.unavailable.contains(&f.id),
                    waiting: status.waiting.iter().find(|w| w.folder == f.id).cloned(),
                    role,
                    local_path: f.local_path,
                    mode: f.mode,
                    remote_id: f.remote_id,
                    drive_id: f.drive_id,
                });
            }
        }
        View {
            signed_in: engine.is_some(),
            server: settings.server.clone(),
            email: settings.email.clone(),
            status,
            paused,
            folders,
            drop_box: settings.drop_box,
            drop_box_path: self.drop_box_dir().to_string_lossy().into_owned(),
            organizer_dir: self.organizer_dir().to_string_lossy().into_owned(),
            autostart: app.autolaunch().is_enabled().unwrap_or(false),
            ignore: settings.ignore(),
            waiting: self.waiting.load(Ordering::SeqCst),
            device_name: system::device_name(),
            on_demand: on_demand(),
        }
    }
}

/// Tell the window and the tray that something changed.
pub async fn refresh(app: &AppHandle) {
    let state = app.state::<App>();
    let view = state.view(app).await;
    tray::update(app, &view);
    let _ = app.emit("state", view);
}

/// Start the engine of the signed-in account, if there is one.
pub async fn start_engine(app: &AppHandle) -> Result<(), String> {
    let state = app.state::<App>();
    let settings = state.settings();
    let (Some(server), Some(email)) = (settings.server.clone(), settings.email.clone()) else {
        return Ok(());
    };
    let Some(token) = system::load_token(&server) else {
        // The credential store lost it (or never had it): sign in again.
        state.update_settings(|s| {
            s.server = None;
            s.email = None;
        });
        return Ok(());
    };
    let mut config = Config::new(&server, &token, &system::device_name());
    config.state = Some(settings::account_dir(&state.data_dir, &server, &email).join("state.db"));
    config.ignore = settings.ignore();
    config.drop_box = settings.drop_box.then(|| state.drop_box_dir());
    let engine = Engine::open(config).await.map_err(|e| e.to_string())?;
    // Unlocked on this computer before: the encrypted folders sync too.
    engine.set_private_key(system::load_private_key(&server));
    engine.start();
    // The extension's session, which the token may have changed since.
    #[cfg(target_os = "macos")]
    if engine
        .folders()
        .await
        .unwrap_or_default()
        .iter()
        .any(|f| f.mode() == organizer_sync::Mode::OnDemand)
    {
        let (server, token) = (server.clone(), token.clone());
        tokio::task::spawn_blocking(move || {
            if let Err(e) = file_provider::session(&server, &token) {
                tracing::warn!("the File Provider extension has no session: {e}");
            }
        });
    }
    *state.engine.lock().await = Some(engine.clone());
    tokio::spawn(follow_status(app.clone(), engine.clone()));
    tokio::spawn(follow_events(app.clone(), engine.clone()));
    tokio::spawn(count_waiting(app.clone(), engine, false));
    refresh(app).await;
    Ok(())
}

/// The engine's status to the window and the tray, at most four times a
/// second.
async fn follow_status(app: AppHandle, engine: Engine) {
    let mut status = engine.status();
    while status.changed().await.is_ok() {
        tokio::time::sleep(Duration::from_millis(250)).await;
        let _ = status.borrow_and_update();
        refresh(&app).await;
    }
}

/// The engine's events: notifications, the drop box's count, sign-out.
async fn follow_events(app: AppHandle, engine: Engine) {
    let mut events = engine.events();
    loop {
        let event = match events.recv().await {
            Ok(event) => event,
            Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => continue,
            Err(_) => return,
        };
        match event {
            Event::Conflict { path, .. } => notify(
                &app,
                "Both versions kept",
                &format!("{path} changed here and on the server: a conflict copy is beside it."),
            ),
            Event::DropBox => {
                let _ = app.emit("drop-box", ());
                count_waiting(app.clone(), engine.clone(), true).await;
            }
            Event::SignedOut => {
                notify(
                    &app,
                    "Signed out",
                    "This computer's sign-in is no longer valid: sign in again.",
                );
                commands::sign_out_here(&app).await;
                return;
            }
            Event::Changes => {
                #[cfg(target_os = "macos")]
                signal_domains(engine.clone());
            }
            // The key here is not the account's any more: forgotten.
            Event::Locked => {
                if let Some(server) = app.state::<App>().settings().server {
                    system::delete_private_key(&server);
                }
                refresh(&app).await;
            }
            Event::Error { .. } => {}
        }
    }
}

/// The on-demand folders' File Provider domains look at the server's
/// changes: once for a burst of live events (half a second after the
/// first, so that the ones after it are seen too).
#[cfg(target_os = "macos")]
fn signal_domains(engine: Engine) {
    use std::sync::atomic::AtomicBool;
    static PENDING: AtomicBool = AtomicBool::new(false);
    if PENDING.swap(true, Ordering::SeqCst) {
        return;
    }
    tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(500)).await;
        PENDING.store(false, Ordering::SeqCst);
        let folders: Vec<String> = engine
            .folders()
            .await
            .unwrap_or_default()
            .into_iter()
            .filter(|f| f.mode() == organizer_sync::Mode::OnDemand)
            .map(|f| f.remote_id)
            .collect();
        let signalled = tokio::task::spawn_blocking(move || file_provider::signal(&folders)).await;
        if let Ok(Err(e)) = signalled {
            tracing::warn!("the File Provider domains were not told: {e}");
        }
    });
}

/// The drop box's items that wait for the user, for the tray; told when
/// there are more than before.
async fn count_waiting(app: AppHandle, engine: Engine, tell: bool) {
    let Ok(waiting) = engine.remote().drop_box_waiting().await else {
        return;
    };
    let state = app.state::<App>();
    let before = state.waiting.swap(waiting, Ordering::SeqCst);
    if tell && waiting > before {
        let body = if waiting == 1 {
            "1 item waits for you in the drop box.".to_string()
        } else {
            format!("{waiting} items wait for you in the drop box.")
        };
        notify(&app, "The drop box has proposals", &body);
    }
    refresh(&app).await;
}

pub fn notify(app: &AppHandle, title: &str, body: &str) {
    if let Err(e) = app.notification().builder().title(title).body(body).show() {
        tracing::warn!("no notification: {e}");
    }
}

/// Show the window, on the page given (`sync`, `drop-box`, `settings`).
pub fn show(app: &AppHandle, page: Option<&str>) {
    if let Some(window) = app.get_webview_window("main") {
        let _ = window.unminimize();
        let _ = window.show();
        let _ = window.set_focus();
    }
    if let Some(page) = page {
        let _ = app.emit("navigate", page);
    }
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "info,organizer_sync=info".into()),
        )
        .init();
    organizer_sync::install_tls_provider();

    tauri::Builder::default()
        // First: a second start shows this one's window instead.
        .plugin(tauri_plugin_single_instance::init(|app, _, _| {
            show(app, None);
        }))
        .plugin(tauri_plugin_autostart::init(
            tauri_plugin_autostart::MacosLauncher::LaunchAgent,
            Some(vec!["--hidden"]),
        ))
        .plugin(tauri_plugin_notification::init())
        .plugin(tauri_plugin_opener::init())
        .setup(|app| {
            #[cfg(target_os = "macos")]
            app.set_activation_policy(tauri::ActivationPolicy::Accessory);
            let config = app.path().app_config_dir()?;
            let data_dir = app.path().app_data_dir()?;
            let home = app.path().home_dir()?;
            let settings_path = config.join("settings.json");
            app.manage(App {
                settings: Mutex::new(Settings::load(&settings_path)),
                settings_path,
                data_dir,
                home,
                engine: tokio::sync::Mutex::new(None),
                waiting: AtomicUsize::new(0),
                tray: Mutex::new(None),
            });
            tray::create(app.handle())?;
            let handle = app.handle().clone();
            let hidden = std::env::args().any(|a| a == "--hidden");
            tauri::async_runtime::spawn(async move {
                if let Err(e) = start_engine(&handle).await {
                    tracing::warn!("the engine did not start: {e}");
                }
                let signed_in = handle.state::<App>().engine.lock().await.is_some();
                if !hidden || !signed_in {
                    show(&handle, None);
                }
                refresh(&handle).await;
            });
            Ok(())
        })
        .on_window_event(|window, event| {
            // Closing the window leaves the app in the tray.
            if let WindowEvent::CloseRequested { api, .. } = event {
                api.prevent_close();
                let _ = window.hide();
            }
        })
        .invoke_handler(tauri::generate_handler![
            commands::app_state,
            commands::sign_in,
            commands::sign_out,
            commands::pause,
            commands::resume,
            commands::add_folder,
            commands::remove_folder,
            commands::move_folder,
            commands::set_drop_box,
            commands::set_autostart,
            commands::set_ignore,
            commands::open_path,
            commands::unlock_encryption,
            commands::lock_encryption,
            commands::api,
        ])
        .run(tauri::generate_context!())
        .expect("the app could not start");
}

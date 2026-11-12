//! What the window asks of the app (`invoke`).

use crate::{App, View, refresh, show, system};
use organizer_sync::{Mode, auth};
use serde::Serialize;
use std::path::{Path, PathBuf};
use std::time::Duration;
use tauri::{AppHandle, Manager, State};
use tauri_plugin_autostart::ManagerExt as _;
use tauri_plugin_opener::OpenerExt as _;

type Result<T> = std::result::Result<T, String>;

fn text(e: impl std::fmt::Display) -> String {
    e.to_string()
}

#[tauri::command]
pub async fn app_state(app: AppHandle, state: State<'_, App>) -> Result<View> {
    Ok(state.view(&app).await)
}

/// Sign in through the system browser (RFC 8252): the server's address,
/// then its sign-in page with a loopback redirect to this app, then the
/// device's token for the one-time code it brings back.
#[tauri::command]
pub async fn sign_in(app: AppHandle, state: State<'_, App>, server: String) -> Result<()> {
    let server = auth::normalize_server(&server).map_err(text)?;
    let pkce = auth::Pkce::new();
    let loopback = auth::Loopback::bind().await.map_err(text)?;
    let device = format!("Organizer on {}", system::device_name());
    let url = auth::authorize_url(&server, &loopback.redirect_uri(), &pkce.challenge, &device);
    // In the log too, for a browser that did not open (the challenge is
    // public: only the verifier, kept here, gets the token).
    tracing::info!("signing in at {url}");
    app.opener()
        .open_url(url, None::<&str>)
        .map_err(|e| format!("The browser could not be opened: {e}"))?;
    let code = loopback
        .code(Duration::from_secs(300))
        .await
        .map_err(text)?;
    let token = auth::exchange(&server, &code, &pkce.verifier)
        .await
        .map_err(text)?;
    let email = auth::whoami(&server, &token).await.map_err(text)?;
    system::save_token(&server, &token)?;
    state.update_settings(|s| {
        s.server = Some(server);
        s.email = Some(email);
    });
    crate::start_engine(&app).await?;
    show(&app, Some("settings"));
    Ok(())
}

/// Sign out: the token is revoked on the server (`DELETE /api/auth/device`)
/// and forgotten here. The synced files stay where they are.
#[tauri::command]
pub async fn sign_out(app: AppHandle, state: State<'_, App>) -> Result<()> {
    let settings = state.settings();
    if let Some(server) = &settings.server
        && let Some(token) = system::load_token(server)
        && let Err(e) = auth::sign_out(server, &token).await
    {
        tracing::warn!("the token could not be revoked on the server: {e}");
    }
    sign_out_here(&app).await;
    Ok(())
}

/// Forget the account here: the engine stops, the token goes (and on
/// macOS the on-demand folders' domains, with the extension's session).
pub async fn sign_out_here(app: &AppHandle) {
    let state = app.state::<App>();
    if let Some(engine) = state.engine.lock().await.take() {
        engine.close().await;
    }
    #[cfg(target_os = "macos")]
    if let Ok(Err(e)) = tokio::task::spawn_blocking(crate::file_provider::sign_out).await {
        tracing::warn!("the File Provider domains were not removed: {e}");
    }
    let settings = state.settings();
    if let Some(server) = &settings.server {
        system::delete_token(server);
        system::delete_private_key(server);
    }
    state.update_settings(|s| {
        s.server = None;
        s.email = None;
    });
    state.waiting.store(0, std::sync::atomic::Ordering::SeqCst);
    refresh(app).await;
}

#[tauri::command]
pub async fn pause(app: AppHandle, state: State<'_, App>) -> Result<()> {
    state.engine().await?.pause();
    refresh(&app).await;
    Ok(())
}

#[tauri::command]
pub async fn resume(app: AppHandle, state: State<'_, App>) -> Result<()> {
    state.engine().await?.resume();
    refresh(&app).await;
    Ok(())
}

/// Sync a folder of the server — a drive's root (My drive's, a shared
/// drive's) or a folder in one — at `local_path`, or `~/Organizer/<name>`
/// (`local_place`). On demand (macOS and Windows), its files
/// download only when opened: on Windows the engine keeps it at that place
/// as a sync root; on macOS it is a File Provider domain, whose place
/// Finder chooses (under Locations).
#[tauri::command]
pub async fn add_folder(
    app: AppHandle,
    state: State<'_, App>,
    remote_id: String,
    name: String,
    local_path: Option<String>,
    mode: Option<Mode>,
) -> Result<()> {
    let engine = state.engine().await?;
    let id = remote_id
        .parse()
        .map_err(|_| "An invalid folder".to_string())?;
    let mode = mode.unwrap_or(Mode::Sync);
    if mode == Mode::OnDemand && crate::on_demand().is_none() {
        return Err(
            "On-demand folders are for macOS and Windows: here, folders sync in full".into(),
        );
    }
    // Before a File Provider domain is made for it: an encrypted folder
    // syncs in full.
    if mode == Mode::OnDemand {
        engine.may_be_on_demand(id).await.map_err(text)?;
    }
    #[cfg(target_os = "macos")]
    if mode == Mode::OnDemand {
        add_domain(&state, &engine, id, &name).await?;
        refresh(&app).await;
        return Ok(());
    }
    let mut taken: Vec<PathBuf> = engine
        .folders()
        .await
        .map_err(text)?
        .iter()
        .map(|f| f.local())
        .collect();
    taken.push(state.drop_box_dir());
    let local = local_place(&state.organizer_dir(), &name, local_path.as_deref(), &taken);
    engine.add_folder(id, &local, mode).await.map_err(text)?;
    refresh(&app).await;
    Ok(())
}

/// An on-demand folder on macOS: its File Provider domain (with the
/// session the extension signs in with), then the engine lists it at the
/// domain's place, and leaves it to the extension.
#[cfg(target_os = "macos")]
async fn add_domain(
    state: &App,
    engine: &organizer_sync::Engine,
    id: uuid::Uuid,
    name: &str,
) -> Result<()> {
    use crate::file_provider;
    let server = state.settings().server.ok_or("Not signed in")?;
    let token = system::load_token(&server).ok_or("Not signed in")?;
    let (folder, name) = (id.to_string(), name.to_string());
    let place = tokio::task::spawn_blocking(move || {
        file_provider::session(&server, &token)?;
        file_provider::add(&folder, &name)
    })
    .await
    .map_err(text)??;
    if let Err(e) = engine.add_folder(id, &place, Mode::OnDemand).await {
        let folder = id.to_string();
        let _ = tokio::task::spawn_blocking(move || file_provider::remove(&folder)).await;
        return Err(e.to_string());
    }
    Ok(())
}

/// Where a folder of the server goes on this computer: the path the user
/// typed, or `~/Organizer/<name>` — a drive as `~/Organizer/<drive name>`
/// (`My drive`), a folder by its name, and the top without a name as
/// `~/Organizer/Files` (where My drive was synced before drives) — with
/// ` 2`, ` 3`... when that is a synced folder's place already, or the drop
/// box folder's (`taken`). The window suggests the same (`lib/drives.ts`).
fn local_place(
    organizer_dir: &Path,
    name: &str,
    typed: Option<&str>,
    taken: &[PathBuf],
) -> PathBuf {
    if let Some(path) = typed.map(str::trim).filter(|p| !p.is_empty()) {
        return PathBuf::from(path);
    }
    let name = if name.is_empty() { "Files" } else { name };
    let overlaps = |place: &Path| {
        taken
            .iter()
            .any(|t| place.starts_with(t) || t.starts_with(place))
    };
    let mut place = organizer_dir.join(name);
    let mut n = 2;
    while overlaps(&place) {
        place = organizer_dir.join(format!("{name} {n}"));
        n += 1;
    }
    place
}

#[tauri::command]
pub async fn remove_folder(app: AppHandle, state: State<'_, App>, id: i64) -> Result<()> {
    let engine = state.engine().await?;
    // On macOS an on-demand folder's domain goes first (what it had not
    // sent yet is kept aside by the system); one that cannot be removed
    // (the app run outside its bundle) does not keep the folder.
    #[cfg(target_os = "macos")]
    if let Some(folder) = engine
        .folders()
        .await
        .map_err(text)?
        .into_iter()
        .find(|f| f.id == id && f.mode() == Mode::OnDemand)
    {
        let remote = folder.remote_id;
        match tokio::task::spawn_blocking(move || crate::file_provider::remove(&remote)).await {
            Ok(Ok(())) => {}
            Ok(Err(e)) => tracing::warn!("the folder's File Provider domain stays: {e}"),
            Err(e) => tracing::warn!("the folder's File Provider domain stays: {e}"),
        }
    }
    engine.remove_folder(id).await.map_err(text)?;
    refresh(&app).await;
    Ok(())
}

/// Move a synced folder's local place (its files move with it).
#[tauri::command]
pub async fn move_folder(
    app: AppHandle,
    state: State<'_, App>,
    id: i64,
    local_path: String,
) -> Result<()> {
    state
        .engine()
        .await?
        .move_folder(id, &PathBuf::from(local_path.trim()))
        .await
        .map_err(text)?;
    refresh(&app).await;
    Ok(())
}

#[tauri::command]
pub async fn set_drop_box(app: AppHandle, state: State<'_, App>, enabled: bool) -> Result<()> {
    state.update_settings(|s| s.drop_box = enabled);
    if let Ok(engine) = state.engine().await {
        engine
            .set_drop_box(enabled.then(|| state.drop_box_dir()))
            .map_err(text)?;
    }
    refresh(&app).await;
    Ok(())
}

#[tauri::command]
pub async fn set_autostart(app: AppHandle, enabled: bool) -> Result<()> {
    let launch = app.autolaunch();
    if enabled {
        launch.enable().map_err(text)?;
    } else {
        launch.disable().map_err(text)?;
    }
    refresh(&app).await;
    Ok(())
}

#[tauri::command]
pub async fn set_ignore(
    app: AppHandle,
    state: State<'_, App>,
    patterns: Vec<String>,
) -> Result<()> {
    let patterns: Vec<String> = patterns
        .into_iter()
        .map(|p| p.trim().to_string())
        .filter(|p| !p.is_empty())
        .collect();
    state.update_settings(|s| s.ignore = Some(patterns.clone()));
    if let Ok(engine) = state.engine().await {
        engine.set_ignore(patterns);
    }
    refresh(&app).await;
    Ok(())
}

/// Open a folder in the system's file manager: a synced one, or
/// `~/Organizer`.
#[tauri::command]
pub async fn open_path(app: AppHandle, state: State<'_, App>, path: Option<String>) -> Result<()> {
    let path = match path {
        Some(path) => PathBuf::from(path),
        None => state.organizer_dir(),
    };
    std::fs::create_dir_all(&path).map_err(text)?;
    app.opener()
        .open_path(path.to_string_lossy(), None::<&str>)
        .map_err(text)
}

/// Unlock the encrypted folders on this computer with the encryption
/// passphrase: the private key goes to the system's credential store, and
/// the engine syncs them from then on.
#[tauri::command]
pub async fn unlock_encryption(
    app: AppHandle,
    state: State<'_, App>,
    passphrase: String,
) -> Result<()> {
    let engine = state.engine().await?;
    let server = state.settings().server.ok_or("Not signed in")?;
    let key = engine.unlock(&passphrase).await.map_err(text)?;
    if let Err(e) = system::save_private_key(&server, &key) {
        engine.set_private_key(None);
        return Err(e);
    }
    refresh(&app).await;
    Ok(())
}

/// Lock them again: the key is forgotten here, and the engine leaves the
/// encrypted folders alone (their files stay where they are).
#[tauri::command]
pub async fn lock_encryption(app: AppHandle, state: State<'_, App>) -> Result<()> {
    if let Ok(engine) = state.engine().await {
        engine.set_private_key(None);
    }
    if let Some(server) = state.settings().server {
        system::delete_private_key(&server);
    }
    refresh(&app).await;
    Ok(())
}

/// An answer of the API, for the window's SDK.
#[derive(Serialize)]
pub struct ApiResponse {
    status: u16,
    content_type: Option<String>,
    body: String,
}

/// A request of the window's SDK, sent with the device's token.
#[tauri::command]
pub async fn api(
    state: State<'_, App>,
    method: String,
    path: String,
    content_type: Option<String>,
    body: Option<String>,
) -> Result<ApiResponse> {
    let engine = state.engine().await?;
    let (status, content_type, body) = engine
        .remote()
        .request(
            &method,
            &path,
            content_type.as_deref(),
            body.map(String::into_bytes),
        )
        .await
        .map_err(text)?;
    Ok(ApiResponse {
        status,
        content_type,
        body: String::from_utf8_lossy(&body).into_owned(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_folder_goes_where_typed_or_under_the_organizer_folder() {
        let dir = Path::new("/home/anna/Organizer");
        assert_eq!(local_place(dir, "Taxes", None, &[]), dir.join("Taxes"));
        assert_eq!(local_place(dir, "", Some("  "), &[]), dir.join("Files"));
        assert_eq!(
            local_place(dir, "My drive", None, &[]),
            dir.join("My drive")
        );
        assert_eq!(
            local_place(dir, "Taxes", Some(" /data/Taxes "), &[]),
            Path::new("/data/Taxes")
        );
    }

    #[test]
    fn a_place_taken_gets_a_number() {
        let dir = Path::new("/home/anna/Organizer");
        let taken = [dir.join("Drop box"), dir.join("Team"), dir.join("Team 2")];
        assert_eq!(
            local_place(dir, "Drop box", None, &taken),
            dir.join("Drop box 2")
        );
        assert_eq!(local_place(dir, "Team", None, &taken), dir.join("Team 3"));
        assert_eq!(local_place(dir, "Teams", None, &taken), dir.join("Teams"));
        // Around a synced folder, too.
        let inside = [dir.join("Files/Taxes")];
        assert_eq!(local_place(dir, "", None, &inside), dir.join("Files 2"));
    }
}

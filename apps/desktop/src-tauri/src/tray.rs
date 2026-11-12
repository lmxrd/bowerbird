//! The tray (the menu bar on macOS): the status, the server and the number
//! of synced folders, then Open Organizer folder, Review the drop box (N),
//! Pause syncing, Settings… and Quit.

use crate::{App, View, show};
use organizer_sync::{Phase, Status};
use tauri::menu::{Menu, MenuItem, PredefinedMenuItem};
use tauri::tray::TrayIconBuilder;
use tauri::{AppHandle, Manager, Wry};

/// The menu's items that change.
pub struct Items {
    status: MenuItem<Wry>,
    detail: MenuItem<Wry>,
    review: MenuItem<Wry>,
    pause: MenuItem<Wry>,
}

pub fn create(app: &AppHandle) -> tauri::Result<()> {
    let status = MenuItem::with_id(app, "status", "Starting…", false, None::<&str>)?;
    let detail = MenuItem::with_id(app, "detail", "", false, None::<&str>)?;
    let open = MenuItem::with_id(app, "open", "Open Organizer folder", true, None::<&str>)?;
    let review = MenuItem::with_id(app, "review", "Review the drop box", true, None::<&str>)?;
    let pause = MenuItem::with_id(app, "pause", "Pause syncing", true, None::<&str>)?;
    let settings = MenuItem::with_id(app, "settings", "Settings…", true, None::<&str>)?;
    let quit = MenuItem::with_id(app, "quit", "Quit", true, None::<&str>)?;
    let menu = Menu::with_items(
        app,
        &[
            &status,
            &detail,
            &PredefinedMenuItem::separator(app)?,
            &open,
            &review,
            &pause,
            &PredefinedMenuItem::separator(app)?,
            &settings,
            &quit,
        ],
    )?;
    let mut tray = TrayIconBuilder::with_id("main")
        .tooltip("Organizer")
        .menu(&menu)
        .show_menu_on_left_click(true)
        .on_menu_event(|app, event| on_menu(app, event.id.as_ref()));
    if let Some(icon) = app.default_window_icon() {
        tray = tray.icon(icon.clone());
    }
    tray.build(app)?;
    *app.state::<App>().tray.lock().unwrap() = Some(Items {
        status,
        detail,
        review,
        pause,
    });
    Ok(())
}

fn on_menu(app: &AppHandle, id: &str) {
    let app = app.clone();
    match id {
        "open" => {
            tauri::async_runtime::spawn(async move {
                let state = app.state::<App>();
                let _ = crate::commands::open_path(app.clone(), state, None).await;
            });
        }
        "review" => show(&app, Some("drop-box")),
        "settings" => show(&app, Some("settings")),
        "pause" => {
            tauri::async_runtime::spawn(async move {
                let state = app.state::<App>();
                if let Ok(engine) = state.engine().await {
                    if engine.is_paused() {
                        engine.resume();
                    } else {
                        engine.pause();
                    }
                }
                crate::refresh(&app).await;
            });
        }
        "quit" => app.exit(0),
        _ => {}
    }
}

/// The status line: Up to date, Syncing N, Paused, Offline, or the error.
pub fn headline(status: &Status, paused: bool) -> String {
    if paused {
        return "Paused".into();
    }
    match status.phase {
        Phase::Paused => "Paused".into(),
        Phase::Offline => "Offline".into(),
        Phase::Error => match &status.error {
            Some(error) => {
                let short: String = error.chars().take(60).collect();
                if short.len() < error.len() {
                    format!("Error: {short}…")
                } else {
                    format!("Error: {short}")
                }
            }
            None => "Error".into(),
        },
        Phase::Syncing if status.pending > 0 => format!("Syncing {}", status.pending),
        Phase::Syncing | Phase::UpToDate => "Up to date".into(),
    }
}

/// The line under it: the server, the folders, and what is not synced
/// (docs/M7B_DESIGN.md §7: kept here, the window says why), and the folders
/// waiting for their drive's storage (docs/M7C_DESIGN.md).
pub fn detail(host: &str, folders: usize, status: &Status) -> String {
    let mut parts = vec![
        host.to_string(),
        match folders {
            1 => "1 folder".to_string(),
            n => format!("{n} folders"),
        },
    ];
    match status.waiting.len() {
        0 => {}
        n => parts.push(format!("{n} waiting for storage")),
    }
    match status.not_synced.len() {
        0 => {}
        n => parts.push(format!("{n} not synced")),
    }
    parts.join(" · ")
}

pub fn update(app: &AppHandle, view: &View) {
    let state = app.state::<App>();
    let tray = state.tray.lock().unwrap();
    let Some(items) = tray.as_ref() else { return };
    if !view.signed_in {
        let _ = items.status.set_text("Not signed in");
        let _ = items.detail.set_text("Sign in from Settings…");
        let _ = items.review.set_text("Review the drop box");
        let _ = items.pause.set_enabled(false);
        let _ = items.review.set_enabled(false);
        return;
    }
    let _ = items.status.set_text(headline(&view.status, view.paused));
    let host = view
        .server
        .as_deref()
        .map(|s| s.split("://").nth(1).unwrap_or(s))
        .unwrap_or_default();
    let _ = items
        .detail
        .set_text(detail(host, view.folders.len(), &view.status));
    let _ = items
        .review
        .set_text(format!("Review the drop box ({})", view.waiting));
    let _ = items.review.set_enabled(true);
    let _ = items.pause.set_enabled(true);
    let _ = items.pause.set_text(if view.paused {
        "Resume syncing"
    } else {
        "Pause syncing"
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_status_line_says_what_the_engine_does() {
        let mut status = Status::default();
        assert_eq!(headline(&status, false), "Up to date");
        status.phase = Phase::Syncing;
        status.pending = 3;
        assert_eq!(headline(&status, false), "Syncing 3");
        assert_eq!(headline(&status, true), "Paused");
        status.phase = Phase::Offline;
        assert_eq!(headline(&status, false), "Offline");
        status.phase = Phase::Error;
        status.error = Some("The server answered 500".into());
        assert_eq!(headline(&status, false), "Error: The server answered 500");
    }

    #[test]
    fn the_detail_line_counts_what_is_not_synced() {
        let mut status = Status::default();
        assert_eq!(
            detail("files.example.com", 1, &status),
            "files.example.com · 1 folder"
        );
        status.not_synced.push(organizer_sync::NotSynced {
            path: "Team/a.txt".into(),
            why: organizer_sync::Held::ViewOnly,
            reason: None,
        });
        assert_eq!(
            detail("files.example.com", 2, &status),
            "files.example.com · 2 folders · 1 not synced"
        );
        status.waiting.push(organizer_sync::Waiting {
            folder: 1,
            why: organizer_sync::Held::Unavailable,
            reason: "Locked until anna@example.com signs in".into(),
        });
        assert_eq!(
            detail("files.example.com", 2, &status),
            "files.example.com · 2 folders · 1 waiting for storage · 1 not synced"
        );
    }
}

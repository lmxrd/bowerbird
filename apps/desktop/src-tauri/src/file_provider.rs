//! On-demand folders on macOS (docs/M5B_DESIGN.md §2): each is a File
//! Provider domain, kept by the extension in the app's bundle
//! (`apps/desktop/macos`), not by the sync engine. File Provider is an
//! Objective-C API, and this crate forbids unsafe code: rather than calling
//! it through FFI, the app runs the small Swift tool beside its executable
//! (`Contents/MacOS/organizer-fileprovider`), one command per call, the
//! token on its standard input, never in its arguments. The calls block:
//! they are made off the async tasks.

use std::io::Write;
use std::path::PathBuf;
use std::process::{Command, Stdio};

/// The tool beside the app's executable; None when the app is not run
/// from its bundle (`cargo run`, `tauri dev`).
fn tool() -> Option<PathBuf> {
    let tool = std::env::current_exe()
        .ok()?
        .parent()?
        .join("organizer-fileprovider");
    tool.exists().then_some(tool)
}

/// Whether the app can have on-demand folders here: the extension and
/// its tool are in the bundle.
pub fn available() -> bool {
    tool().is_some()
}

fn run(args: &[&str], input: Option<&[u8]>) -> Result<String, String> {
    let tool = tool().ok_or_else(|| {
        "On-demand folders need the installed app (its File Provider extension is in the bundle)"
            .to_string()
    })?;
    let mut child = Command::new(tool)
        .args(args)
        .stdin(if input.is_some() {
            Stdio::piped()
        } else {
            Stdio::null()
        })
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| e.to_string())?;
    if let (Some(input), Some(mut stdin)) = (input, child.stdin.take()) {
        stdin.write_all(input).map_err(|e| e.to_string())?;
    }
    let output = child.wait_with_output().map_err(|e| e.to_string())?;
    if output.status.success() {
        Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
    } else {
        Err(explain(String::from_utf8_lossy(&output.stderr).trim()))
    }
}

/// The system's refusal of an extension that cannot load (the app is not
/// signed with an Apple team yet) said in words a user can act on.
fn explain(error: &str) -> String {
    if error.contains("NSFileProviderErrorDomain") && error.contains("-2001") {
        format!(
            "On-demand folders need the app signed with an Apple team (see the README's \
             Desktop section): {error}"
        )
    } else {
        error.to_string()
    }
}

/// The name Finder shows for a folder of the server (its top: Files).
pub fn display_name(name: &str) -> String {
    if name.is_empty() {
        "Files".into()
    } else {
        name.into()
    }
}

/// Add the domain of a folder of the server: its place in Finder.
pub fn add(folder: &str, name: &str) -> Result<PathBuf, String> {
    run(&["add", folder, &display_name(name)], None).map(PathBuf::from)
}

/// Remove a folder's domain: what was not sent yet is kept aside by the
/// system (in the Trash's "Recovered" files).
pub fn remove(folder: &str) -> Result<(), String> {
    run(&["remove", folder], None).map(|_| ())
}

/// Signing out: every domain goes, and the extension's session.
pub fn sign_out() -> Result<(), String> {
    if !available() {
        return Ok(());
    }
    run(&["remove-all"], None)?;
    run(&["forget"], None).map(|_| ())
}

/// The working sets look at the server's changes (a live event came).
pub fn signal(folders: &[String]) -> Result<(), String> {
    if folders.is_empty() {
        return Ok(());
    }
    let mut args = vec!["signal"];
    args.extend(folders.iter().map(String::as_str));
    run(&args, None).map(|_| ())
}

/// The server and the token, for the extension (in the Keychain group the
/// tool and the extension share).
pub fn session(server: &str, token: &str) -> Result<(), String> {
    let json = serde_json::json!({ "server": server, "token": token });
    run(&["session"], Some(json.to_string().as_bytes())).map(|_| ())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_top_of_the_tree_shows_as_files() {
        assert_eq!(display_name(""), "Files");
        assert_eq!(display_name("Taxes"), "Taxes");
    }

    #[test]
    fn an_extension_that_cannot_load_says_why() {
        let refused = "Error Domain=NSFileProviderErrorDomain Code=-2001 \"The application cannot be used right now.\"";
        assert!(explain(refused).starts_with("On-demand folders need the app signed"));
        assert_eq!(explain("No domain for x"), "No domain for x");
    }

    #[test]
    fn without_the_bundle_there_is_no_tool() {
        // The tests run from the target directory, not from a bundle.
        assert!(!available());
        assert!(add("id", "Taxes").is_err());
        assert!(sign_out().is_ok());
    }
}

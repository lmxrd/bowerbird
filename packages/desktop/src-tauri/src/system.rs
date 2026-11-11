//! What the app needs from the system: the token and the unlocked private
//! key of the encrypted folders in its credential store (Keychain,
//! Credential Manager, Secret Service), and this computer's name.

use organizer_sync::PrivateKey;

/// The credential store's service for the app's tokens, one per server.
const SERVICE: &str = "eu.vardalabs.organizer.desktop";
/// And for the private keys that unlock the encrypted folders, one per
/// server: never in a file, nor in the web view.
const KEY_SERVICE: &str = "eu.vardalabs.organizer.desktop.e2e";

pub fn save_token(server: &str, token: &str) -> Result<(), String> {
    keyring::Entry::new(SERVICE, server)
        .and_then(|entry| entry.set_password(token))
        .map_err(|e| format!("The token could not be kept in the system's credential store: {e}"))
}

pub fn load_token(server: &str) -> Option<String> {
    keyring::Entry::new(SERVICE, server)
        .and_then(|entry| entry.get_password())
        .ok()
}

pub fn delete_token(server: &str) {
    if let Ok(entry) = keyring::Entry::new(SERVICE, server) {
        let _ = entry.delete_credential();
    }
}

pub fn save_private_key(server: &str, key: &PrivateKey) -> Result<(), String> {
    keyring::Entry::new(KEY_SERVICE, server)
        .and_then(|entry| entry.set_password(&key.encode()))
        .map_err(|e| format!("The key could not be kept in the system's credential store: {e}"))
}

pub fn load_private_key(server: &str) -> Option<PrivateKey> {
    keyring::Entry::new(KEY_SERVICE, server)
        .and_then(|entry| entry.get_password())
        .ok()
        .and_then(|text| PrivateKey::decode(&text))
}

pub fn delete_private_key(server: &str) {
    if let Ok(entry) = keyring::Entry::new(KEY_SERVICE, server) {
        let _ = entry.delete_credential();
    }
}

/// This computer's name, for the device list and the conflict copies
/// (`Report (conflict, MacBook, 2026-11-07).docx`): what the system calls
/// it, as the user set it. Asked once: the window's state carries it.
pub fn device_name() -> String {
    static NAME: std::sync::OnceLock<String> = std::sync::OnceLock::new();
    NAME.get_or_init(read_device_name).clone()
}

fn read_device_name() -> String {
    let name = if cfg!(windows) {
        std::env::var("COMPUTERNAME").ok()
    } else if cfg!(target_os = "macos") {
        std::process::Command::new("scutil")
            .args(["--get", "ComputerName"])
            .output()
            .ok()
            .and_then(|o| String::from_utf8(o.stdout).ok())
    } else {
        std::fs::read_to_string("/etc/hostname").ok()
    };
    name.map(|n| n.trim().replace(['/', '\\', ':'], "-"))
        .filter(|n| !n.is_empty())
        .unwrap_or_else(|| "Desktop".into())
}

//! What the app remembers between runs: the account (the server and who is
//! signed in) and the user's choices, in `settings.json` in the app's
//! config folder. The token is not here: it is in the system's credential
//! store (`token`), and the synced folders are in the engine's state.

use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    /// The server's address, while signed in.
    pub server: Option<String>,
    /// Who is signed in (their email), for the settings.
    pub email: Option<String>,
    /// Whether the drop box folder (`~/Organizer/Drop box`) is on.
    pub drop_box: bool,
    /// The names never synced; None: the engine's default list.
    pub ignore: Option<Vec<String>>,
}

impl Settings {
    pub fn load(path: &Path) -> Settings {
        std::fs::read(path)
            .ok()
            .and_then(|bytes| serde_json::from_slice(&bytes).ok())
            .unwrap_or(Settings {
                drop_box: true,
                ..Settings::default()
            })
    }

    pub fn save(&self, path: &Path) -> std::io::Result<()> {
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        let temp = path.with_extension("json.tmp");
        std::fs::write(&temp, serde_json::to_vec_pretty(self)?)?;
        std::fs::rename(temp, path)
    }

    pub fn ignore(&self) -> Vec<String> {
        self.ignore.clone().unwrap_or_else(|| {
            organizer_sync::ignore::DEFAULT
                .iter()
                .map(|s| s.to_string())
                .collect()
        })
    }
}

/// The folder of an account's state: one per server and user, so that
/// signing in again picks up where it was.
pub fn account_dir(data: &Path, server: &str, email: &str) -> PathBuf {
    let name: String = format!("{server}-{email}")
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '.' || c == '-' {
                c
            } else {
                '_'
            }
        })
        .collect();
    data.join("accounts").join(name)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_account_has_a_folder_of_safe_characters() {
        let dir = account_dir(
            Path::new("/data"),
            "https://files.example.com:8443",
            "anna@example.com",
        );
        assert_eq!(
            dir,
            Path::new("/data/accounts/https___files.example.com_8443-anna_example.com")
        );
    }

    #[test]
    fn settings_start_with_the_drop_box_folder_on() {
        let settings = Settings::load(Path::new("/nonexistent/settings.json"));
        assert!(settings.drop_box);
        assert!(settings.ignore().contains(&".DS_Store".to_string()));
    }
}

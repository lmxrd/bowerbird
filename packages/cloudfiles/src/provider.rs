//! [`Placeholders`] on Windows, through `cfapi`: one sync root per
//! on-demand folder, connected to the engine's [`Fetch`] while the app
//! runs.

use crate::{Fetch, Placeholder, PlaceholderState, Placeholders, cfapi, pin_action};
use std::collections::HashMap;
use std::io;
use std::os::windows::fs::MetadataExt;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

pub struct CloudFiles {
    fetch: Arc<dyn Fetch>,
    connections: Mutex<HashMap<PathBuf, cfapi::Connection>>,
}

impl CloudFiles {
    pub fn new(fetch: Arc<dyn Fetch>) -> Self {
        CloudFiles {
            fetch,
            connections: Mutex::new(HashMap::new()),
        }
    }
}

fn icon() -> String {
    std::env::current_exe()
        .map(|exe| format!("{},0", exe.display()))
        .unwrap_or_default()
}

impl Placeholders for CloudFiles {
    fn connect(&self, root: &Path, account: &str, name: &str) -> io::Result<()> {
        let mut connections = self.connections.lock().unwrap();
        if connections.contains_key(root) {
            return Ok(());
        }
        std::fs::create_dir_all(root)?;
        let id = crate::sync_root_id(&cfapi::user_sid()?, account);
        // Registering again updates it (a new name, the app moved).
        cfapi::register(&cfapi::Registration {
            path: root,
            id: &id,
            display_name: name,
            icon: &icon(),
            version: env!("CARGO_PKG_VERSION"),
        })?;
        let connection = cfapi::connect(root, self.fetch.clone())?;
        connections.insert(root.to_path_buf(), connection);
        Ok(())
    }

    fn release(&self, root: &Path, account: &str) -> io::Result<()> {
        // Files first: what was downloaded stays, as a plain file; what was
        // not goes (the server has it), so nothing is left that cannot open.
        let mut stack = vec![root.to_path_buf()];
        while let Some(dir) = stack.pop() {
            let Ok(entries) = std::fs::read_dir(&dir) else {
                continue;
            };
            for entry in entries.flatten() {
                let path = entry.path();
                let Ok(metadata) = std::fs::symlink_metadata(&path) else {
                    continue;
                };
                if metadata.is_dir() {
                    stack.push(path);
                } else if cfapi::state(&path)?.is_some() {
                    if crate::hydrated(metadata.file_attributes()) {
                        cfapi::revert(&path)?;
                    } else {
                        std::fs::remove_file(&path)?;
                    }
                }
            }
        }
        // Disconnected (dropped) before it is unregistered.
        self.connections.lock().unwrap().remove(root);
        cfapi::unregister(&crate::sync_root_id(&cfapi::user_sid()?, account))
    }

    fn create(&self, path: &Path, file: &Placeholder) -> io::Result<()> {
        cfapi::create(path, file)
    }

    fn state(&self, path: &Path) -> io::Result<Option<PlaceholderState>> {
        let attributes = std::fs::symlink_metadata(path)?.file_attributes();
        Ok(cfapi::state(path)?.map(|state| PlaceholderState {
            hydrated: crate::hydrated(attributes),
            pin: pin_action(attributes),
            ..state
        }))
    }

    fn update(&self, path: &Path, file: &Placeholder) -> io::Result<()> {
        cfapi::update(path, file)
    }

    fn adopt(&self, path: &Path, file: &Placeholder) -> io::Result<()> {
        cfapi::adopt(path, file)
    }

    fn hydrate(&self, path: &Path) -> io::Result<()> {
        cfapi::hydrate(path)
    }

    fn dehydrate(&self, path: &Path) -> io::Result<()> {
        cfapi::dehydrate(path)
    }
}

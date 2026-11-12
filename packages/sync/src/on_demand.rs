//! On-demand folders (docs/M5B_DESIGN.md): their files show with their
//! name and size, but their content comes down only when opened, and can
//! be freed again.
//!
//! On Windows the engine keeps them itself, as placeholders of the Cloud
//! Files API (`organizer-cloudfiles`): the passes are those of a full-sync
//! folder, but a file of the server becomes a placeholder rather than a
//! download, a placeholder is never hashed (that would download it), and
//! what was downloaded or sent becomes a placeholder in sync. The system
//! asks for a placeholder's bytes through [`Fetcher`]. On macOS the File
//! Provider extension keeps them (`apps/desktop/macos`), and the engine
//! leaves such a folder alone.

use crate::remote::Remote;
use std::fmt;
use std::io;
use std::sync::Arc;
use uuid::Uuid;

pub use organizer_cloudfiles::{
    Fetch, Identity, PinAction, Placeholder, PlaceholderState, Placeholders,
};

/// A system of placeholders, for [`crate::Config`].
#[derive(Clone)]
pub struct Provider(pub Arc<dyn Placeholders>);

impl fmt::Debug for Provider {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("Provider")
    }
}

/// This system's placeholders: Windows' Cloud Files; none elsewhere.
#[cfg(windows)]
pub fn system(remote: &Remote) -> Option<Arc<dyn Placeholders>> {
    let fetcher = Fetcher::new(remote.clone(), tokio::runtime::Handle::current());
    Some(Arc::new(organizer_cloudfiles::CloudFiles::new(Arc::new(
        fetcher,
    ))))
}

#[cfg(not(windows))]
pub fn system(_: &Remote) -> Option<Arc<dyn Placeholders>> {
    None
}

/// What a placeholder of a node at a version carries.
pub fn identity(id: Uuid, version: i64) -> Identity {
    Identity {
        id: *id.as_bytes(),
        version,
    }
}

/// An RFC 3339 time in Unix nanoseconds; 0 when it is not one.
pub fn unix_ns(rfc3339: &str) -> i64 {
    rfc3339
        .parse::<jiff::Timestamp>()
        .map(|t| t.as_nanosecond() as i64)
        .unwrap_or(0)
}

/// A placeholder's bytes from the server: a range of the file, at the
/// version the placeholder shows (another version is an error: the next
/// pass shows the new one). Called on the system's threads, never on the
/// engine's: it waits for the download on the runtime.
pub struct Fetcher {
    remote: Remote,
    runtime: tokio::runtime::Handle,
}

impl Fetcher {
    pub fn new(remote: Remote, runtime: tokio::runtime::Handle) -> Self {
        Fetcher { remote, runtime }
    }
}

impl Fetch for Fetcher {
    fn fetch(
        &self,
        identity: Identity,
        offset: u64,
        length: u64,
        write: &mut dyn FnMut(&[u8]) -> io::Result<()>,
    ) -> io::Result<()> {
        if length == 0 {
            return Ok(());
        }
        let id = Uuid::from_bytes(identity.id);
        self.runtime.block_on(async {
            let mut response = self
                .remote
                .download_range(id, identity.version, offset, length)
                .await
                .map_err(io::Error::other)?;
            while let Some(chunk) = response.chunk().await.map_err(io::Error::other)? {
                write(&chunk)?;
            }
            Ok(())
        })
    }
}

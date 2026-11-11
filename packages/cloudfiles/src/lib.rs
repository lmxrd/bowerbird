//! Windows' Cloud Files API (`cfapi`, the one OneDrive uses), for the sync
//! engine's on-demand folders (docs/M5B_DESIGN.md §3). An on-demand folder
//! is a sync root; each file of the server is a placeholder there, with its
//! name, size and time, whose content the system asks for (a fetch
//! callback) when the file is opened, and frees again ("Free up space").
//!
//! What is pure is here and tested on any system: a placeholder's identity,
//! the ranges a fetch sends, what a pin state asks for. [`Placeholders`] is
//! what the engine needs of the system, [`Fetch`] what the system needs of
//! the engine. `CloudFiles` (Windows only) is the one implementation, on
//! `cfapi.rs`: the one module with unsafe code, which the calls into
//! Windows need (the `windows` crate's Cloud Files functions are all
//! `unsafe`, and the callbacks are `extern "system"`).

use std::io;
use std::path::Path;

#[cfg(windows)]
mod cfapi;
#[cfg(windows)]
mod provider;
#[cfg(windows)]
pub use provider::CloudFiles;

/// What a placeholder carries to tell whose it is: the server's node and
/// the version it shows, so that a fetch asks for that one.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Identity {
    pub id: [u8; 16],
    pub version: i64,
}

impl Identity {
    /// The format's first byte, should another ever be needed.
    const FORMAT: u8 = 1;
    pub const LEN: usize = 25;

    pub fn to_bytes(self) -> [u8; Self::LEN] {
        let mut out = [0; Self::LEN];
        out[0] = Self::FORMAT;
        out[1..17].copy_from_slice(&self.id);
        out[17..].copy_from_slice(&self.version.to_le_bytes());
        out
    }

    /// None for bytes that are not an identity of ours.
    pub fn from_bytes(bytes: &[u8]) -> Option<Self> {
        if bytes.len() != Self::LEN || bytes[0] != Self::FORMAT {
            return None;
        }
        Some(Identity {
            id: bytes[1..17].try_into().ok()?,
            version: i64::from_le_bytes(bytes[17..].try_into().ok()?),
        })
    }
}

/// A file of the server as a placeholder shows it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Placeholder {
    pub identity: Identity,
    pub size: u64,
    /// The server's modification time, Unix nanoseconds.
    pub modified_ns: i64,
}

/// What a local file is as a placeholder.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PlaceholderState {
    /// None when it is someone else's (or an older format).
    pub identity: Option<Identity>,
    /// As the server's: the system clears it when the file is written to.
    pub in_sync: bool,
    /// Its content is here (not only its name and size).
    pub hydrated: bool,
    /// What the user asked for ("Always keep on this device", "Free up
    /// space").
    pub pin: PinAction,
}

/// What the engine needs of the system, per on-demand folder and file.
/// Every call may block (the system's calls do): the engine makes them
/// off its async tasks.
pub trait Placeholders: Send + Sync {
    /// Make `root` a sync root (shown in the file manager as `name`) and
    /// answer its fetches, at the engine's start or when the folder is
    /// added. `account` tells this folder's sync root from the others.
    fn connect(&self, root: &Path, account: &str, name: &str) -> io::Result<()>;
    /// The folder is no longer on demand: its downloaded files become
    /// plain files, the others go (the server has them), and the sync root
    /// is unregistered.
    fn release(&self, root: &Path, account: &str) -> io::Result<()>;
    /// A placeholder, not downloaded, for a file of the server.
    fn create(&self, path: &Path, file: &Placeholder) -> io::Result<()>;
    /// What is at `path` as a placeholder; None for a plain file.
    fn state(&self, path: &Path) -> io::Result<Option<PlaceholderState>>;
    /// The server's file changed and this one was not downloaded: it shows
    /// the new one, still not downloaded.
    fn update(&self, path: &Path, file: &Placeholder) -> io::Result<()>;
    /// The file at `path`, as it is, is the server's `file` now (it was
    /// just sent or downloaded): a placeholder in sync, which can be freed.
    fn adopt(&self, path: &Path, file: &Placeholder) -> io::Result<()>;
    /// Download a placeholder's content (it was pinned).
    fn hydrate(&self, path: &Path) -> io::Result<()>;
    /// Free a placeholder's content (it was unpinned).
    fn dehydrate(&self, path: &Path) -> io::Result<()>;
}

/// What the system needs of the engine: a placeholder's bytes. `write`
/// takes them in order, from `offset`, until `length` came.
pub trait Fetch: Send + Sync + 'static {
    fn fetch(
        &self,
        identity: Identity,
        offset: u64,
        length: u64,
        write: &mut dyn FnMut(&[u8]) -> io::Result<()>,
    ) -> io::Result<()>;
}

/// What a file's pin state asks of the engine.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PinAction {
    Nothing,
    /// Pinned ("Always keep on this device") and not here: download it.
    Hydrate,
    /// Unpinned ("Free up space") and here: free it.
    Dehydrate,
}

/// The file attributes of placeholders (`winnt.h`).
pub mod attributes {
    pub const PINNED: u32 = 0x0008_0000;
    pub const UNPINNED: u32 = 0x0010_0000;
    pub const RECALL_ON_OPEN: u32 = 0x0004_0000;
    pub const RECALL_ON_DATA_ACCESS: u32 = 0x0040_0000;
}

/// Whether a file of these attributes has its content here.
pub fn hydrated(attributes: u32) -> bool {
    attributes & (attributes::RECALL_ON_DATA_ACCESS | attributes::RECALL_ON_OPEN) == 0
}

/// What a file of these attributes asks for.
pub fn pin_action(attributes: u32) -> PinAction {
    let here = hydrated(attributes);
    if attributes & attributes::PINNED != 0 && !here {
        PinAction::Hydrate
    } else if attributes & attributes::UNPINNED != 0 && here {
        PinAction::Dehydrate
    } else {
        PinAction::Nothing
    }
}

/// The unit of a transfer: a piece's offset and length are multiples of
/// it, but for the last piece of the file.
pub const BLOCK: u64 = 4096;

/// What a fetch sends for the range the system asked for: widened to whole
/// blocks, within the file. `(offset, length)`.
pub fn transfer_range(offset: u64, length: u64, size: u64) -> (u64, u64) {
    let start = (offset / BLOCK * BLOCK).min(size);
    let end = offset
        .saturating_add(length)
        .div_ceil(BLOCK)
        .saturating_mul(BLOCK)
        .min(size);
    (start, end.saturating_sub(start))
}

/// The bytes of a fetch, cut into the pieces a transfer takes: whole
/// blocks of `piece` bytes as they come, then what is left at the end
/// (which [`transfer_range`] made a whole block, or the end of the file).
pub struct Chunker {
    next: u64,
    end: u64,
    piece: usize,
    buffer: Vec<u8>,
}

impl Chunker {
    pub fn new(offset: u64, length: u64, piece: usize) -> Self {
        let block = BLOCK as usize;
        Chunker {
            next: offset,
            end: offset + length,
            piece: piece.div_ceil(block).max(1) * block,
            buffer: Vec::new(),
        }
    }

    /// The bytes that came: the pieces ready to send, `(offset, bytes)`.
    /// Bytes past the range are dropped.
    pub fn push(&mut self, bytes: &[u8]) -> Vec<(u64, Vec<u8>)> {
        let room = (self.end - self.next) as usize - self.buffer.len();
        self.buffer
            .extend_from_slice(&bytes[..bytes.len().min(room)]);
        let mut out = Vec::new();
        while self.buffer.len() >= self.piece {
            let rest = self.buffer.split_off(self.piece);
            let piece = std::mem::replace(&mut self.buffer, rest);
            let at = self.next;
            self.next += piece.len() as u64;
            out.push((at, piece));
        }
        out
    }

    /// The last piece, once everything came; an error when the bytes
    /// stopped short of the range.
    pub fn finish(self) -> io::Result<Option<(u64, Vec<u8>)>> {
        if self.next + self.buffer.len() as u64 != self.end {
            return Err(io::Error::new(
                io::ErrorKind::UnexpectedEof,
                "the download stopped short",
            ));
        }
        Ok((!self.buffer.is_empty()).then_some((self.next, self.buffer)))
    }
}

/// A Windows time (100 ns since 1601) of a Unix time in nanoseconds.
pub fn file_time(unix_ns: i64) -> i64 {
    const UNIX_EPOCH: i64 = 116_444_736_000_000_000;
    unix_ns / 100 + UNIX_EPOCH
}

/// A sync root's id, as the system wants it: `provider!user SID!account`.
pub fn sync_root_id(sid: &str, account: &str) -> String {
    let account: String = account
        .chars()
        .map(|c| if c == '!' { '_' } else { c })
        .collect();
    format!("Organizer!{sid}!{account}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_identity_round_trips_and_a_foreign_one_is_none() {
        let identity = Identity {
            id: *b"0123456789abcdef",
            version: 42,
        };
        assert_eq!(Identity::from_bytes(&identity.to_bytes()), Some(identity));
        assert_eq!(Identity::from_bytes(b"OneDrive's own"), None);
        let mut other = identity.to_bytes();
        other[0] = 9;
        assert_eq!(Identity::from_bytes(&other), None);
    }

    #[test]
    fn a_fetch_sends_whole_blocks_within_the_file() {
        // Inside a block: the block.
        assert_eq!(transfer_range(100, 10, 1_000_000), (0, 4096));
        // Across blocks: both, whole.
        assert_eq!(transfer_range(4000, 200, 1_000_000), (0, 8192));
        // At the end: up to the end of the file, a short last piece.
        assert_eq!(transfer_range(8192, 4096, 10_000), (8192, 1808));
        // All of a small file.
        assert_eq!(transfer_range(0, 12, 12), (0, 12));
        // Past the end: nothing.
        assert_eq!(transfer_range(20_000, 10, 10_000), (10_000, 0));
    }

    #[test]
    fn bytes_go_in_whole_pieces_and_the_rest_at_the_end() {
        let mut chunker = Chunker::new(8192, 10_000, 4096);
        assert!(chunker.push(&[1; 3000]).is_empty());
        let pieces = chunker.push(&[2; 3000]);
        assert_eq!(pieces.len(), 1);
        assert_eq!(pieces[0].0, 8192);
        assert_eq!(pieces[0].1.len(), 4096);
        let pieces = chunker.push(&[3; 5000]);
        assert_eq!(pieces.len(), 1);
        assert_eq!(pieces[0].0, 8192 + 4096);
        // 1808 left of the range; what came past it is dropped.
        let (at, last) = chunker.finish().unwrap().unwrap();
        assert_eq!((at, last.len()), (8192 + 8192, 1808));
    }

    #[test]
    fn a_fetch_that_stops_short_is_an_error() {
        let mut chunker = Chunker::new(0, 5000, 4096);
        chunker.push(&[0; 4096]);
        assert!(chunker.finish().is_err());
        let chunker = Chunker::new(0, 0, 4096);
        assert_eq!(chunker.finish().unwrap(), None);
    }

    #[test]
    fn a_piece_is_whole_blocks() {
        let mut chunker = Chunker::new(0, 3 * BLOCK, 5000);
        let pieces = chunker.push(&[0; 3 * BLOCK as usize]);
        assert_eq!(pieces.len(), 1);
        assert_eq!(pieces[0].1.len(), 2 * BLOCK as usize);
    }

    #[test]
    fn pins_ask_for_a_download_or_a_free() {
        use attributes::*;
        assert_eq!(
            pin_action(PINNED | RECALL_ON_DATA_ACCESS),
            PinAction::Hydrate
        );
        assert_eq!(pin_action(PINNED), PinAction::Nothing);
        assert_eq!(pin_action(UNPINNED), PinAction::Dehydrate);
        assert_eq!(
            pin_action(UNPINNED | RECALL_ON_DATA_ACCESS),
            PinAction::Nothing
        );
        assert_eq!(pin_action(0), PinAction::Nothing);
        assert!(!hydrated(RECALL_ON_DATA_ACCESS));
        assert!(hydrated(0x20));
    }

    #[test]
    fn times_and_ids() {
        assert_eq!(file_time(0), 116_444_736_000_000_000);
        assert_eq!(file_time(1_000_000_000), 116_444_736_010_000_000);
        assert_eq!(
            sync_root_id("S-1-5-21-1", "a!b"),
            "Organizer!S-1-5-21-1!a_b"
        );
    }
}

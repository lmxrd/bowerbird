//! The calls into Windows: the Cloud Files functions, the fetch callback,
//! the sync root's registration and the user's SID. The one module with
//! unsafe code (the crate denies it elsewhere): the `windows` crate's Win32
//! functions are all `unsafe`, as their pointers and handles are the
//! caller's to keep valid, and a callback is an `extern "system"` function
//! the system calls with raw pointers. Each `unsafe` block says what keeps
//! it sound; everything above this module is safe.
#![allow(unsafe_code)]

use crate::{Chunker, Fetch, Identity, Placeholder, PlaceholderState, file_time};
use std::ffi::c_void;
use std::io;
use std::os::windows::io::AsRawHandle;
use std::path::Path;
use std::sync::Arc;
use windows::Storage::Provider::{
    StorageProviderHardlinkPolicy, StorageProviderHydrationPolicy,
    StorageProviderHydrationPolicyModifier, StorageProviderInSyncPolicy,
    StorageProviderPopulationPolicy, StorageProviderSyncRootInfo, StorageProviderSyncRootManager,
};
use windows::Storage::StorageFolder;
use windows::Win32::Foundation::{
    CloseHandle, HANDLE, HLOCAL, LocalFree, NTSTATUS, STATUS_SUCCESS, STATUS_UNSUCCESSFUL,
};
use windows::Win32::Security::Authorization::ConvertSidToStringSidW;
use windows::Win32::Security::{GetTokenInformation, TOKEN_QUERY, TOKEN_USER, TokenUser};
use windows::Win32::Storage::CloudFilters::*;
use windows::Win32::Storage::FileSystem::FILE_BASIC_INFO;
use windows::Win32::System::Threading::{GetCurrentProcess, OpenProcessToken};
use windows::core::{HRESULT, HSTRING, PCWSTR, PWSTR};

fn other(e: windows::core::Error) -> io::Error {
    io::Error::other(e)
}

fn wide(path: &Path) -> HSTRING {
    HSTRING::from(path.as_os_str())
}

/// `HRESULT_FROM_WIN32(ERROR_NOT_A_CLOUD_FILE)`: a plain file.
const NOT_A_CLOUD_FILE: HRESULT = HRESULT(0x8007_0178_u32 as i32);

// --- The sync root ---------------------------------------------------------

/// What a sync root is registered with.
pub struct Registration<'a> {
    pub path: &'a Path,
    pub id: &'a str,
    pub display_name: &'a str,
    /// `<exe>,0`: the app's icon.
    pub icon: &'a str,
    pub version: &'a str,
}

/// Register a sync root with the shell (the navigation pane's entry, the
/// cloud icons, "Free up space" and "Always keep on this device"); it also
/// registers it with the Cloud Files filter. WinRT, safe calls: the async
/// lookup of the folder is waited for, as this runs off the engine's tasks.
pub fn register(r: &Registration) -> io::Result<()> {
    let folder = StorageFolder::GetFolderFromPathAsync(&wide(r.path))
        .and_then(|op| op.join())
        .map_err(other)?;
    let info = StorageProviderSyncRootInfo::new().map_err(other)?;
    (|| {
        info.SetId(&HSTRING::from(r.id))?;
        info.SetPath(&folder)?;
        info.SetDisplayNameResource(&HSTRING::from(r.display_name))?;
        info.SetIconResource(&HSTRING::from(r.icon))?;
        info.SetVersion(&HSTRING::from(r.version))?;
        // The whole file when it is opened; the system may free files on
        // its own when space is short (Storage Sense).
        info.SetHydrationPolicy(StorageProviderHydrationPolicy::Full)?;
        info.SetHydrationPolicyModifier(
            StorageProviderHydrationPolicyModifier::AutoDehydrationAllowed,
        )?;
        // Every placeholder is made by the engine from the server's tree,
        // so the system never asks for a folder's listing.
        info.SetPopulationPolicy(StorageProviderPopulationPolicy::AlwaysFull)?;
        info.SetInSyncPolicy(StorageProviderInSyncPolicy::Default)?;
        info.SetHardlinkPolicy(StorageProviderHardlinkPolicy::None)?;
        info.SetShowSiblingsAsGroup(false)?;
        StorageProviderSyncRootManager::Register(&info)
    })()
    .map_err(other)
}

pub fn unregister(id: &str) -> io::Result<()> {
    StorageProviderSyncRootManager::Unregister(&HSTRING::from(id)).map_err(other)
}

/// The signed-in user's SID (`S-1-5-21-…`), which a sync root's id holds.
pub fn user_sid() -> io::Result<String> {
    let mut token = HANDLE::default();
    // SAFETY: the current process' pseudo-handle needs no closing; `token`
    // is written by the call and closed below.
    unsafe { OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut token) }.map_err(other)?;
    let result = (|| {
        let mut needed = 0u32;
        // SAFETY: a null buffer of length 0 only asks for the size.
        let _ = unsafe { GetTokenInformation(token, TokenUser, None, 0, &mut needed) };
        // u64s: aligned for TOKEN_USER, whose SID points into the buffer.
        let mut buffer = vec![0u64; (needed as usize).div_ceil(8).max(1)];
        // SAFETY: the buffer is `needed` bytes or more, and stays alive while
        // the SID inside it is read.
        unsafe {
            GetTokenInformation(
                token,
                TokenUser,
                Some(buffer.as_mut_ptr().cast()),
                (buffer.len() * 8) as u32,
                &mut needed,
            )
        }
        .map_err(other)?;
        // SAFETY: the call filled a TOKEN_USER at the buffer's start.
        let user = unsafe { &*(buffer.as_ptr() as *const TOKEN_USER) };
        let mut text = PWSTR::null();
        // SAFETY: the SID is valid (in `buffer`); the string the call
        // allocates is copied, then freed with LocalFree as documented.
        unsafe {
            ConvertSidToStringSidW(user.User.Sid, &mut text).map_err(other)?;
            let sid = text.to_string().map_err(io::Error::other);
            LocalFree(Some(HLOCAL(text.0.cast())));
            sid
        }
    })();
    // SAFETY: opened above, closed once.
    let _ = unsafe { CloseHandle(token) };
    result
}

// --- Fetches ---------------------------------------------------------------

/// A connection of the sync root to its fetch callback: dropped, it is
/// disconnected (the callbacks stop) and its context freed.
pub struct Connection {
    key: CF_CONNECTION_KEY,
    context: *mut Arc<dyn Fetch>,
}

// SAFETY: the key is a number, and the context is only read (by the
// callbacks, on the system's threads) until the drop, which disconnects
// first: no callback runs once CfDisconnectSyncRoot has returned.
unsafe impl Send for Connection {}
unsafe impl Sync for Connection {}

impl Drop for Connection {
    fn drop(&mut self) {
        // SAFETY: the key is this connection's; after the disconnect no
        // callback reads the context, which `connect` made with Box.
        unsafe {
            let _ = CfDisconnectSyncRoot(self.key);
            drop(Box::from_raw(self.context));
        }
    }
}

/// The callbacks: only the fetch of a file's content (the population
/// policy is "always full", so no listing is ever asked for).
static CALLBACKS: [CF_CALLBACK_REGISTRATION; 2] = [
    CF_CALLBACK_REGISTRATION {
        Type: CF_CALLBACK_TYPE_FETCH_DATA,
        Callback: Some(on_fetch_data),
    },
    CF_CALLBACK_REGISTRATION {
        Type: CF_CALLBACK_TYPE_NONE,
        Callback: None,
    },
];

pub fn connect(path: &Path, fetch: Arc<dyn Fetch>) -> io::Result<Connection> {
    let context = Box::into_raw(Box::new(fetch));
    // SAFETY: the table is static; the context stays valid until the
    // Connection's drop disconnects.
    let key = unsafe {
        CfConnectSyncRoot(
            &wide(path),
            CALLBACKS.as_ptr(),
            Some(context as *const c_void),
            CF_CONNECT_FLAG_REQUIRE_FULL_FILE_PATH,
        )
    };
    match key {
        Ok(key) => Ok(Connection { key, context }),
        Err(e) => {
            // SAFETY: not connected: nothing else has the context.
            drop(unsafe { Box::from_raw(context) });
            Err(other(e))
        }
    }
}

/// What a transfer answers: the keys of the request it answers.
struct Transfer {
    connection: CF_CONNECTION_KEY,
    transfer: i64,
    request: i64,
    correlation: *const windows::Win32::System::CorrelationVector::CORRELATION_VECTOR,
}

impl Transfer {
    fn execute(&self, status: NTSTATUS, bytes: &[u8], offset: u64, length: u64) -> io::Result<()> {
        let info = CF_OPERATION_INFO {
            StructSize: size_of::<CF_OPERATION_INFO>() as u32,
            Type: CF_OPERATION_TYPE_TRANSFER_DATA,
            ConnectionKey: self.connection,
            TransferKey: self.transfer,
            CorrelationVector: self.correlation,
            SyncStatus: std::ptr::null(),
            RequestKey: self.request,
        };
        let mut params = CF_OPERATION_PARAMETERS {
            ParamSize: size_of::<CF_OPERATION_PARAMETERS>() as u32,
            Anonymous: CF_OPERATION_PARAMETERS_0 {
                TransferData: CF_OPERATION_PARAMETERS_0_0 {
                    Flags: CF_OPERATION_TRANSFER_DATA_FLAG_NONE,
                    CompletionStatus: status,
                    Buffer: if bytes.is_empty() {
                        std::ptr::null()
                    } else {
                        bytes.as_ptr().cast()
                    },
                    Offset: offset as i64,
                    Length: length as i64,
                },
            },
        };
        // SAFETY: the keys are the callback's, still running; the bytes
        // outlive the call, which copies them.
        unsafe { CfExecute(&info, &mut params) }.map_err(other)
    }

    fn send(&self, offset: u64, bytes: &[u8]) -> io::Result<()> {
        self.execute(STATUS_SUCCESS, bytes, offset, bytes.len() as u64)
    }

    /// The range could not be fetched: the opening fails, and is tried
    /// again by the next one.
    fn fail(&self, offset: u64, length: u64) {
        let _ = self.execute(STATUS_UNSUCCESSFUL, &[], offset, length);
    }
}

/// How much one transfer sends at most (whole blocks).
const PIECE: usize = 1024 * 1024;

/// The system asks for a range of a placeholder's content: it is
/// downloaded (`Fetch`) and handed over a piece at a time, on the system's
/// thread (each piece keeps the request alive).
unsafe extern "system" fn on_fetch_data(
    info: *const CF_CALLBACK_INFO,
    params: *const CF_CALLBACK_PARAMETERS,
) {
    // A panic must not unwind into the system.
    let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        // SAFETY: the system passes valid pointers for the callback's
        // duration, and the fetch parameters for a fetch callback; the
        // context is the Arc that `connect` gave, alive while connected;
        // the identity is the bytes the placeholder was made with.
        let (info, fetch, required, identity) = unsafe {
            let info = &*info;
            let fetch = &*(info.CallbackContext as *const Arc<dyn Fetch>);
            let required = (*params).Anonymous.FetchData;
            let identity = if info.FileIdentity.is_null() {
                &[][..]
            } else {
                std::slice::from_raw_parts(
                    info.FileIdentity as *const u8,
                    info.FileIdentityLength as usize,
                )
            };
            (info, fetch, required, identity)
        };
        let transfer = Transfer {
            connection: info.ConnectionKey,
            transfer: info.TransferKey,
            request: info.RequestKey,
            correlation: info.CorrelationVector,
        };
        let size = info.FileSize.max(0) as u64;
        let (offset, length) = crate::transfer_range(
            required.RequiredFileOffset.max(0) as u64,
            required.RequiredLength.max(0) as u64,
            size,
        );
        let result = match Identity::from_bytes(identity) {
            Some(identity) => {
                let mut chunker = Chunker::new(offset, length, PIECE);
                fetch
                    .fetch(identity, offset, length, &mut |bytes| {
                        for (at, piece) in chunker.push(bytes) {
                            transfer.send(at, &piece)?;
                        }
                        Ok(())
                    })
                    .and_then(|()| match chunker.finish()? {
                        Some((at, piece)) => transfer.send(at, &piece),
                        None => Ok(()),
                    })
            }
            None => Err(io::Error::other("not a placeholder of Organizer's")),
        };
        if result.is_err() {
            transfer.fail(offset, length);
        }
    }));
}

// --- Placeholders ----------------------------------------------------------

/// A file opened through the Cloud Files filter (with an oplock), which
/// does not download it; closed when dropped.
struct Protected(HANDLE);

impl Protected {
    fn open(path: &Path, write: bool) -> io::Result<Self> {
        let flags = if write {
            CF_OPEN_FILE_FLAG_EXCLUSIVE | CF_OPEN_FILE_FLAG_WRITE_ACCESS
        } else {
            CF_OPEN_FILE_FLAG_NONE
        };
        // SAFETY: the path is a valid wide string for the call.
        unsafe { CfOpenFileWithOplock(&wide(path), flags) }
            .map(Protected)
            .map_err(other)
    }

    fn handle(&self) -> HANDLE {
        // SAFETY: the protected handle is open until the drop.
        unsafe { CfGetWin32HandleFromProtectedHandle(self.0) }
    }
}

impl Drop for Protected {
    fn drop(&mut self) {
        // SAFETY: opened by CfOpenFileWithOplock, closed once.
        unsafe { CfCloseHandle(self.0) };
    }
}

fn metadata(file: &Placeholder) -> CF_FS_METADATA {
    let time = file_time(file.modified_ns);
    CF_FS_METADATA {
        BasicInfo: FILE_BASIC_INFO {
            CreationTime: time,
            LastAccessTime: time,
            LastWriteTime: time,
            ChangeTime: time,
            // FILE_ATTRIBUTE_NORMAL
            FileAttributes: 0x80,
        },
        FileSize: file.size as i64,
    }
}

/// A placeholder, not downloaded and in sync, for a file of the server.
pub fn create(path: &Path, file: &Placeholder) -> io::Result<()> {
    let (Some(dir), Some(name)) = (path.parent(), path.file_name()) else {
        return Err(io::Error::other("a placeholder needs a folder and a name"));
    };
    let name = HSTRING::from(name);
    let identity = file.identity.to_bytes();
    let mut info = [CF_PLACEHOLDER_CREATE_INFO {
        RelativeFileName: PCWSTR(name.as_ptr()),
        FsMetadata: metadata(file),
        FileIdentity: identity.as_ptr().cast(),
        FileIdentityLength: identity.len() as u32,
        Flags: CF_PLACEHOLDER_CREATE_FLAG_MARK_IN_SYNC,
        Result: HRESULT(0),
        CreateUsn: 0,
    }];
    // SAFETY: the name and the identity outlive the call.
    unsafe { CfCreatePlaceholders(&wide(dir), &mut info, CF_CREATE_FLAG_NONE, None) }
        .map_err(other)?;
    info[0].Result.ok().map_err(other)
}

/// What a file is as a placeholder: its identity and whether it is in
/// sync (the pin and hydration states are the caller's, from the file's
/// attributes); None for a plain file.
pub fn state(path: &Path) -> io::Result<Option<PlaceholderState>> {
    let file = Protected::open(path, false)?;
    // The basic info, then the identity's bytes; ours are 25.
    let mut buffer = vec![0u64; (size_of::<CF_PLACEHOLDER_BASIC_INFO>() + 512).div_ceil(8)];
    let length = (buffer.len() * 8) as u32;
    let mut returned = 0u32;
    // SAFETY: the buffer is `length` bytes, aligned for the struct.
    let result = unsafe {
        CfGetPlaceholderInfo(
            file.handle(),
            CF_PLACEHOLDER_INFO_BASIC,
            buffer.as_mut_ptr().cast(),
            length,
            Some(&mut returned),
        )
    };
    match result {
        Err(e) if e.code() == NOT_A_CLOUD_FILE => return Ok(None),
        Err(e) => return Err(other(e)),
        Ok(()) => {}
    }
    // SAFETY: the call filled a CF_PLACEHOLDER_BASIC_INFO at the start;
    // the identity's bytes follow it, within the bytes returned.
    let (in_sync, identity) = unsafe {
        let info = &*(buffer.as_ptr() as *const CF_PLACEHOLDER_BASIC_INFO);
        let start = std::mem::offset_of!(CF_PLACEHOLDER_BASIC_INFO, FileIdentity);
        let len = (info.FileIdentityLength as usize).min((returned as usize).saturating_sub(start));
        let bytes = std::slice::from_raw_parts((buffer.as_ptr() as *const u8).add(start), len);
        (
            info.InSyncState == CF_IN_SYNC_STATE_IN_SYNC,
            Identity::from_bytes(bytes),
        )
    };
    Ok(Some(PlaceholderState {
        identity,
        in_sync,
        // The caller's, from the attributes.
        hydrated: false,
        pin: crate::PinAction::Nothing,
    }))
}

/// A placeholder shows another version of the server's file: its content
/// goes (it is fetched again when opened), in sync.
pub fn update(path: &Path, file: &Placeholder) -> io::Result<()> {
    let handle = Protected::open(path, true)?;
    let metadata = metadata(file);
    let identity = file.identity.to_bytes();
    // SAFETY: the metadata and identity outlive the call; the handle is open.
    unsafe {
        CfUpdatePlaceholder(
            handle.handle(),
            Some(&metadata),
            Some(identity.as_ptr().cast()),
            identity.len() as u32,
            None,
            CF_UPDATE_FLAG_MARK_IN_SYNC | CF_UPDATE_FLAG_DEHYDRATE,
            None,
            None,
        )
    }
    .map_err(other)
}

/// The file as it is becomes the server's `file`, in sync: a plain file is
/// converted, a placeholder takes the new identity.
pub fn adopt(path: &Path, file: &Placeholder) -> io::Result<()> {
    let identity = file.identity.to_bytes();
    if state(path)?.is_some() {
        let handle = Protected::open(path, true)?;
        // SAFETY: the identity outlives the call; the handle is open.
        return unsafe {
            CfUpdatePlaceholder(
                handle.handle(),
                None,
                Some(identity.as_ptr().cast()),
                identity.len() as u32,
                None,
                CF_UPDATE_FLAG_MARK_IN_SYNC,
                None,
                None,
            )
        }
        .map_err(other);
    }
    let plain = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(path)?;
    // SAFETY: the std file's handle is open until `plain` drops, after the
    // call; the identity outlives it.
    unsafe {
        CfConvertToPlaceholder(
            HANDLE(plain.as_raw_handle()),
            Some(identity.as_ptr().cast()),
            identity.len() as u32,
            CF_CONVERT_FLAG_MARK_IN_SYNC,
            None,
            None,
        )
    }
    .map_err(other)
}

/// A downloaded placeholder becomes a plain file again.
pub fn revert(path: &Path) -> io::Result<()> {
    let handle = Protected::open(path, true)?;
    // SAFETY: the handle is open.
    unsafe { CfRevertPlaceholder(handle.handle(), CF_REVERT_FLAG_NONE, None) }.map_err(other)
}

/// Download all of a placeholder (the system calls our fetch callback).
pub fn hydrate(path: &Path) -> io::Result<()> {
    let handle = Protected::open(path, false)?;
    // SAFETY: the handle is open; -1: to the end.
    unsafe { CfHydratePlaceholder(handle.handle(), 0, -1, CF_HYDRATE_FLAG_NONE, None) }
        .map_err(other)
}

/// Free all of a placeholder's content.
pub fn dehydrate(path: &Path) -> io::Result<()> {
    let handle = Protected::open(path, true)?;
    // SAFETY: the handle is open, exclusively; -1: to the end.
    unsafe { CfDehydratePlaceholder(handle.handle(), 0, -1, CF_DEHYDRATE_FLAG_NONE, None) }
        .map_err(other)
}

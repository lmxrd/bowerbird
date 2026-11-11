//! A file's contents: a header (version, the folder key's generation, a
//! random file nonce), then the contents in 64 KiB segments, each sealed
//! with AES-256-GCM under a key of this file alone, with the segment's
//! number and a last-segment flag in its nonce: segments cannot be
//! reordered, dropped, cut or added. docs/E2E_SPEC.md §5.

use crate::{Error, VERSION, hkdf, open, seal};
use std::io::{Read, Write};

/// The plaintext bytes of a segment; the last one has 0 to this many.
pub const SEGMENT: usize = 65_536;
/// The tag each sealed segment ends with.
pub const TAG: usize = 16;
/// A sealed segment that is not the last.
pub const SEALED_SEGMENT: usize = SEGMENT + TAG;
/// Version (1), generation (4, big-endian), file nonce (32).
pub const HEADER_LEN: usize = 1 + 4 + 32;
/// The HKDF info of a file key.
pub const FILE_INFO: &[u8] = b"organizer-e2e/1/file";

/// What the header says.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Header {
    pub generation: u32,
    pub file_nonce: [u8; 32],
}

impl Header {
    pub fn to_bytes(self) -> [u8; HEADER_LEN] {
        let mut out = [0u8; HEADER_LEN];
        out[0] = VERSION;
        out[1..5].copy_from_slice(&self.generation.to_be_bytes());
        out[5..].copy_from_slice(&self.file_nonce);
        out
    }

    pub fn parse(bytes: &[u8]) -> Result<Header, Error> {
        if bytes.len() < HEADER_LEN {
            return Err(Error::Malformed("shorter than the header"));
        }
        if bytes[0] != VERSION {
            return Err(Error::Version(bytes[0]));
        }
        Ok(Header {
            generation: u32::from_be_bytes(bytes[1..5].try_into().expect("4 bytes")),
            file_nonce: bytes[5..HEADER_LEN].try_into().expect("32 bytes"),
        })
    }
}

/// The key of one file: HKDF of the folder key, salted with the file nonce.
pub fn file_key(folder_key: &[u8; 32], file_nonce: &[u8; 32]) -> [u8; 32] {
    hkdf(folder_key, file_nonce, FILE_INFO)
}

/// A segment's nonce: seven zero bytes, its number (4 bytes, big-endian)
/// and 1 for the last segment, else 0.
pub fn segment_nonce(index: u32, last: bool) -> [u8; 12] {
    let mut nonce = [0u8; 12];
    nonce[7..11].copy_from_slice(&index.to_be_bytes());
    nonce[11] = u8::from(last);
    nonce
}

pub fn seal_segment(file_key: &[u8; 32], index: u32, last: bool, plain: &[u8]) -> Vec<u8> {
    debug_assert!(plain.len() <= SEGMENT);
    seal(file_key, &segment_nonce(index, last), plain, &[])
}

pub fn open_segment(
    file_key: &[u8; 32],
    index: u32,
    last: bool,
    sealed: &[u8],
) -> Result<Vec<u8>, Error> {
    open(file_key, &segment_nonce(index, last), sealed, &[])
}

/// How many segments a plaintext of `len` bytes has: at least one (an
/// empty file is one empty last segment).
pub fn segments(len: u64) -> u64 {
    len.div_ceil(SEGMENT as u64).max(1)
}

/// The size of the encrypted file of a plaintext of `len` bytes.
pub fn encrypted_size(len: u64) -> u64 {
    HEADER_LEN as u64 + len + TAG as u64 * segments(len)
}

/// The size of the plaintext in an encrypted file of `len` bytes.
pub fn plain_size(len: u64) -> Result<u64, Error> {
    let body = len
        .checked_sub((HEADER_LEN + TAG) as u64)
        .ok_or(Error::Malformed("shorter than a header and a segment"))?
        + TAG as u64;
    let full = body / SEALED_SEGMENT as u64;
    let rest = body % SEALED_SEGMENT as u64;
    match rest {
        0 => Ok(full * SEGMENT as u64),
        r if r < TAG as u64 => Err(Error::Malformed("a segment shorter than its tag")),
        r => Ok(full * SEGMENT as u64 + r - TAG as u64),
    }
}

/// A whole plaintext encrypted, with the file nonce given.
pub fn encrypt_with(
    folder_key: &[u8; 32],
    generation: u32,
    file_nonce: &[u8; 32],
    plain: &[u8],
) -> Vec<u8> {
    let mut out = Vec::with_capacity(encrypted_size(plain.len() as u64) as usize);
    encrypt_io(
        folder_key,
        generation,
        file_nonce,
        &mut &plain[..],
        &mut out,
    )
    .expect("writing to memory");
    out
}

/// A whole plaintext encrypted, with a new file nonce.
pub fn encrypt(folder_key: &[u8; 32], generation: u32, plain: &[u8]) -> Vec<u8> {
    encrypt_with(folder_key, generation, &crate::random_32(), plain)
}

/// A whole file decrypted, with the key of its generation (`keys`).
pub fn decrypt(keys: impl Fn(u32) -> Option<[u8; 32]>, encrypted: &[u8]) -> Result<Vec<u8>, Error> {
    let mut out = Vec::new();
    decrypt_io(keys, &mut &encrypted[..], &mut out).map_err(|e| match e {
        IoError::Crypto(e) => e,
        IoError::Io(_) => Error::Malformed("unreadable"),
    })?;
    Ok(out)
}

/// What can go wrong while streaming.
#[derive(Debug, thiserror::Error)]
pub enum IoError {
    #[error(transparent)]
    Crypto(#[from] Error),
    #[error(transparent)]
    Io(#[from] std::io::Error),
}

/// Encrypt from a reader to a writer, a segment at a time. Answers the
/// bytes written.
pub fn encrypt_io(
    folder_key: &[u8; 32],
    generation: u32,
    file_nonce: &[u8; 32],
    from: &mut impl Read,
    to: &mut impl Write,
) -> std::io::Result<u64> {
    let header = Header {
        generation,
        file_nonce: *file_nonce,
    };
    to.write_all(&header.to_bytes())?;
    let key = file_key(folder_key, file_nonce);
    let mut written = HEADER_LEN as u64;
    let mut current = vec![0u8; SEGMENT];
    let mut filled = read_full(from, &mut current)?;
    let mut index: u32 = 0;
    loop {
        // A full segment is the last one only when nothing follows it.
        let mut next = vec![0u8; SEGMENT];
        let next_filled = if filled == SEGMENT {
            read_full(from, &mut next)?
        } else {
            0
        };
        let last = next_filled == 0;
        let sealed = seal_segment(&key, index, last, &current[..filled]);
        to.write_all(&sealed)?;
        written += sealed.len() as u64;
        if last {
            return Ok(written);
        }
        index = index
            .checked_add(1)
            .ok_or_else(|| std::io::Error::other("too many segments"))?;
        current = next;
        filled = next_filled;
    }
}

/// Decrypt from a reader to a writer, a segment at a time, with the key of
/// the file's generation (`keys`). Answers the bytes written. A writer may
/// have received the segments before a failure: write to a temporary file.
pub fn decrypt_io(
    keys: impl Fn(u32) -> Option<[u8; 32]>,
    from: &mut impl Read,
    to: &mut impl Write,
) -> Result<u64, IoError> {
    let mut header = [0u8; HEADER_LEN];
    if read_full(from, &mut header)? < HEADER_LEN {
        return Err(Error::Malformed("shorter than the header").into());
    }
    let header = Header::parse(&header)?;
    let folder_key = keys(header.generation).ok_or(Error::NoKey(header.generation))?;
    let key = file_key(&folder_key, &header.file_nonce);
    let mut written = 0u64;
    let mut current = vec![0u8; SEALED_SEGMENT];
    let mut filled = read_full(from, &mut current)?;
    let mut index: u32 = 0;
    loop {
        if filled < TAG {
            return Err(Error::Malformed("a segment shorter than its tag").into());
        }
        let mut next = vec![0u8; SEALED_SEGMENT];
        let next_filled = if filled == SEALED_SEGMENT {
            read_full(from, &mut next)?
        } else {
            0
        };
        let last = next_filled == 0;
        let plain = open_segment(&key, index, last, &current[..filled])?;
        to.write_all(&plain)?;
        written += plain.len() as u64;
        if last {
            return Ok(written);
        }
        index = index
            .checked_add(1)
            .ok_or(Error::Malformed("too many segments"))?;
        current = next;
        filled = next_filled;
    }
}

/// Fill `buf` as far as the reader goes; answers how much was read.
fn read_full(from: &mut impl Read, buf: &mut [u8]) -> std::io::Result<usize> {
    let mut filled = 0;
    while filled < buf.len() {
        match from.read(&mut buf[filled..]) {
            Ok(0) => break,
            Ok(n) => filled += n,
            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => {}
            Err(e) => return Err(e),
        }
    }
    Ok(filled)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pattern(len: usize) -> Vec<u8> {
        (0..len).map(|i| (i % 251) as u8).collect()
    }

    #[test]
    fn contents_of_every_size_read_back() {
        let key = crate::random_32();
        for len in [
            0,
            1,
            SEGMENT - 1,
            SEGMENT,
            SEGMENT + 1,
            2 * SEGMENT,
            3 * SEGMENT + 17,
        ] {
            let plain = pattern(len);
            let sealed = encrypt(&key, 4, &plain);
            assert_eq!(sealed.len() as u64, encrypted_size(len as u64), "{len}");
            assert_eq!(plain_size(sealed.len() as u64).unwrap(), len as u64);
            assert_eq!(Header::parse(&sealed).unwrap().generation, 4);
            let keys = |g| (g == 4).then_some(key);
            assert_eq!(decrypt(keys, &sealed).unwrap(), plain, "{len}");
        }
    }

    #[test]
    fn segments_cannot_be_cut_reordered_or_added() {
        let key = crate::random_32();
        let keys = |_| Some(key);
        let plain = pattern(2 * SEGMENT + 10);
        let sealed = encrypt(&key, 1, &plain);
        let first = HEADER_LEN..HEADER_LEN + SEALED_SEGMENT;
        let second = first.end..first.end + SEALED_SEGMENT;
        // Cut after a full segment: it was not the last.
        assert_eq!(decrypt(keys, &sealed[..second.end]), Err(Error::Decrypt));
        // The first two swapped.
        let mut swapped = sealed[..HEADER_LEN].to_vec();
        swapped.extend_from_slice(&sealed[second.clone()]);
        swapped.extend_from_slice(&sealed[first.clone()]);
        swapped.extend_from_slice(&sealed[second.end..]);
        assert_eq!(decrypt(keys, &swapped), Err(Error::Decrypt));
        // A byte changed.
        let mut changed = sealed.clone();
        changed[HEADER_LEN + 3] ^= 1;
        assert_eq!(decrypt(keys, &changed), Err(Error::Decrypt));
        // Another key.
        assert_eq!(
            decrypt(|_| Some(crate::random_32()), &sealed),
            Err(Error::Decrypt)
        );
        assert_eq!(decrypt(|_| None, &sealed), Err(Error::NoKey(1)));
    }
}

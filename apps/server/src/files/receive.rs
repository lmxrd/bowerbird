//! Receiving a request body into a file, a frame at a time: never the whole
//! body in memory.

use super::FileError;
use axum::body::{Body, HttpBody};
use std::path::Path;
use std::pin::Pin;
use tokio::io::AsyncWriteExt;

/// Write a body to `path` (created, or appended to with `append`), at most
/// `max` bytes in all: more is `TooLarge`, and what was written stays for
/// the caller to remove. Answers the bytes written.
pub async fn receive(body: Body, path: &Path, append: bool, max: u64) -> Result<u64, FileError> {
    let mut file = tokio::fs::OpenOptions::new()
        .create(true)
        .write(true)
        .append(append)
        .truncate(!append)
        .open(path)
        .await?;
    let mut body = body;
    let mut written = 0u64;
    while let Some(frame) = std::future::poll_fn(|cx| Pin::new(&mut body).poll_frame(cx)).await {
        let frame = frame.map_err(|e| anyhow::anyhow!("the request body broke off: {e}"))?;
        let Ok(data) = frame.into_data() else {
            continue;
        };
        written += data.len() as u64;
        if written > max {
            return Err(FileError::TooLarge(max));
        }
        file.write_all(&data).await?;
    }
    file.flush().await?;
    file.sync_all().await?;
    Ok(written)
}

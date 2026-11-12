//! The embeddings of a user's tree, kept in the [`VectorStore`]: each
//! folder (its path and a few file names), and the text read from files
//! for search, in chunks. Only with an embeddings model; made again when it
//! changes. They follow the tree after the search index does
//! (`search::run`), a round of texts at a time.
//!
//! This sends the folder names and the text of files to the embeddings
//! endpoint: the settings pages say so ("What Organizer sends to the AI").

use crate::drop_box::retrieve;
use crate::state::AppState;
use crate::vectors::{Entry, VectorKind};
use std::collections::{HashMap, HashSet};
use uuid::Uuid;

/// The length of a chunk of text, in characters.
pub const CHUNK_CHARS: usize = 1_200;
/// The most chunks of one file.
pub const MAX_CHUNKS: usize = 16;
/// How many texts are embedded in one request.
const BATCH: usize = 32;
/// How many texts one round embeds, before the tree is looked at again.
const PER_ROUND: usize = 256;

/// A text's fingerprint: its BLAKE3, with the model's name.
fn fingerprint(model: &str, text: &str) -> String {
    blake3::hash(format!("{model}\n{text}").as_bytes())
        .to_hex()
        .to_string()
}

/// A text in pieces of about [`CHUNK_CHARS`], cut at a line or a space
/// where there is one, at most [`MAX_CHUNKS`].
pub fn chunks(text: &str) -> Vec<String> {
    let chars: Vec<char> = text.chars().collect();
    let mut out = Vec::new();
    let mut start = 0;
    while start < chars.len() && out.len() < MAX_CHUNKS {
        let mut end = (start + CHUNK_CHARS).min(chars.len());
        if end < chars.len() {
            // Back to the last line break, else space, in the second half.
            let floor = start + CHUNK_CHARS / 2;
            if let Some(at) = (floor..end).rev().find(|&i| chars[i] == '\n') {
                end = at + 1;
            } else if let Some(at) = (floor..end).rev().find(|&i| chars[i] == ' ') {
                end = at + 1;
            }
        }
        let chunk: String = chars[start..end].iter().collect();
        let chunk = chunk.trim().to_string();
        if !chunk.is_empty() {
            out.push(chunk);
        }
        start = end;
    }
    out
}

/// A folder as it is embedded.
pub fn folder_text(folder: &retrieve::Folder) -> String {
    if folder.examples.is_empty() {
        folder.display()
    } else {
        format!("{}\n{}", folder.display(), folder.examples.join("\n"))
    }
}

/// Bring a user's vectors up to date, a round of texts. Answers whether
/// more are left.
pub async fn sync(state: &AppState, user: Uuid) -> anyhow::Result<bool> {
    let Some(model) = state.ai.model_for(crate::ai::Work::Embeddings) else {
        return Ok(false);
    };
    let model = model.model;
    let store = &state.vectors;
    if store.model().await?.as_deref() != Some(model.as_str()) {
        store.reset(&model).await?;
    }
    let mut budget = PER_ROUND;

    // The folders.
    let folders = retrieve::folders_of(&state.db, user).await?;
    let have: HashMap<String, String> = store
        .fingerprints(user, VectorKind::Folder)
        .await?
        .into_iter()
        .collect();
    let mut wanted = HashSet::new();
    let mut pending: Vec<(String, Uuid, String, String)> = Vec::new();
    for folder in folders.iter().filter(|f| !f.path.is_empty()) {
        let key = folder.id.to_string();
        let text = folder_text(folder);
        let print = fingerprint(&model, &text);
        if have.get(&key) != Some(&print) {
            pending.push((key.clone(), folder.id, print, text));
        }
        wanted.insert(key);
    }
    let gone: Vec<String> = have
        .keys()
        .filter(|k| !wanted.contains(*k))
        .cloned()
        .collect();
    store.remove(user, VectorKind::Folder, &gone).await?;
    let more_folders = pending.len() > budget;
    pending.truncate(budget);
    budget -= pending.len();
    embed_and_put(state, user, VectorKind::Folder, pending).await?;

    // The text of files, by its content hash.
    let files: Vec<(Uuid, Vec<u8>)> = sqlx::query_as(
        "SELECT d.node_id, d.text_hash FROM search_docs d JOIN search s ON s.rowid = d.rowid
         WHERE d.drive_id = ?1 AND d.kind = 'file' AND d.text_hash IS NOT NULL
           AND length(s.text) > 0",
    )
    .bind(user)
    .fetch_all(state.db.reader())
    .await?;
    let have: HashMap<String, String> = store
        .fingerprints(user, VectorKind::Chunk)
        .await?
        .into_iter()
        .collect();
    let mut wanted = HashSet::new();
    let mut more_files = false;
    let mut pending = Vec::new();
    for (node, hash) in files {
        let print = format!("{}:{}", hex::encode(&hash), fingerprint(&model, ""));
        let first = format!("{node}:0");
        if have.get(&first) == Some(&print) {
            // Up to date: its chunks stay.
            wanted.extend(
                have.keys()
                    .filter(|k| k.starts_with(&format!("{node}:")))
                    .cloned(),
            );
            continue;
        }
        if budget == 0 {
            more_files = true;
            // Kept until they are made again.
            wanted.extend(
                have.keys()
                    .filter(|k| k.starts_with(&format!("{node}:")))
                    .cloned(),
            );
            continue;
        }
        let (name, path, text): (String, String, String) = sqlx::query_as(
            "SELECT d.name, d.path, s.text FROM search_docs d JOIN search s ON s.rowid = d.rowid
             WHERE d.node_id = ?1",
        )
        .bind(node)
        .fetch_one(state.db.reader())
        .await?;
        let parts = chunks(&text);
        for (n, chunk) in parts.iter().enumerate() {
            let key = format!("{node}:{n}");
            // Each piece knows where it is from.
            pending.push((
                key.clone(),
                node,
                print.clone(),
                format!("{path}/{name}\n{chunk}"),
            ));
            wanted.insert(key);
        }
        budget = budget.saturating_sub(parts.len());
    }
    let gone: Vec<String> = have
        .keys()
        .filter(|k| !wanted.contains(*k))
        .cloned()
        .collect();
    store.remove(user, VectorKind::Chunk, &gone).await?;
    embed_and_put(state, user, VectorKind::Chunk, pending).await?;
    Ok(more_folders || more_files)
}

async fn embed_and_put(
    state: &AppState,
    user: Uuid,
    kind: VectorKind,
    pending: Vec<(String, Uuid, String, String)>,
) -> anyhow::Result<()> {
    for batch in pending.chunks(BATCH) {
        let texts: Vec<String> = batch.iter().map(|(_, _, _, text)| text.clone()).collect();
        let vectors = state.ai.embed(texts).await?;
        let entries: Vec<Entry> = batch
            .iter()
            .zip(vectors)
            .map(|((key, node, print, _), vector)| Entry {
                user_id: user,
                kind,
                key: key.clone(),
                node_id: *node,
                fingerprint: print.clone(),
                vector,
            })
            .collect();
        state.vectors.put(&entries).await?;
    }
    Ok(())
}

/// The folders nearest to an item's text, for the proposal; empty without
/// embeddings or when the endpoint fails (the lexical ones are enough).
pub async fn nearest_folders(state: &AppState, user: Uuid, text: &str, k: usize) -> Vec<Uuid> {
    if !state.ai.on(crate::ai::Work::Embeddings) || text.trim().is_empty() {
        return Vec::new();
    }
    let query = match state.ai.embed(vec![text.to_string()]).await {
        Ok(mut v) if !v.is_empty() => v.remove(0),
        Ok(_) => return Vec::new(),
        Err(e) => {
            tracing::info!("cannot embed a drop box item: {e}");
            return Vec::new();
        }
    };
    match state
        .vectors
        .nearest(user, VectorKind::Folder, &query, k)
        .await
    {
        Ok(near) => near.into_iter().map(|n| n.node_id).collect(),
        Err(e) => {
            tracing::warn!("cannot search the folder vectors: {e}");
            Vec::new()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn text_is_cut_at_lines_or_spaces() {
        assert!(chunks("").is_empty());
        assert_eq!(chunks("short"), ["short"]);
        let long = "word ".repeat(1000);
        let parts = chunks(&long);
        assert!(parts.len() > 3);
        assert!(parts.iter().all(|p| p.chars().count() <= CHUNK_CHARS));
        assert!(parts.iter().all(|p| p.ends_with("word")));
        assert_eq!(chunks(&"x".repeat(CHUNK_CHARS * 40)).len(), MAX_CHUNKS);
    }
}

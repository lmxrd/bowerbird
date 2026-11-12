//! The text of a file, through xberg: PDF (its native extractor), office
//! documents, spreadsheets, HTML and plain text.
//!
//! xberg is only ever handed the file's bytes, never a path or a URL (it
//! would fetch a URL itself), and its cache is off (it would keep the text
//! on disk outside the data dir).

use crate::storage::Storage;
use std::path::Path;
use std::time::Duration;

/// Files larger than this are not read: their name and type must do.
pub const MAX_READ_BYTES: u64 = 50_000_000;
/// The most text kept of a file, in characters: what the model is sent.
pub const MAX_CHARS: usize = 32_000;
/// The longest an extraction may take.
const TIMEOUT: Duration = Duration::from_secs(60);

/// Why there is no text.
#[derive(Debug, PartialEq)]
pub enum NoText {
    /// Larger than [`MAX_READ_BYTES`].
    TooLarge,
    /// A kind of file without text (an image, an archive, ...), or one with
    /// none in it.
    Empty,
    /// xberg failed, panicked or took too long: the reason.
    Failed(String),
}

/// The text of a file at `path` of a storage (a drive's, or a staged
/// file's: `Storage::local_file`), at most `max_chars` characters, with
/// runs of blank lines and spaces squeezed.
pub async fn text_of(
    storage: &Storage,
    path: &str,
    name: &str,
    mime_type: &str,
    max_chars: usize,
) -> Result<String, NoText> {
    let (size, _) = storage
        .file_meta(path)
        .await
        .map_err(|e| NoText::Failed(e.to_string()))?;
    if size as u64 > MAX_READ_BYTES {
        return Err(NoText::TooLarge);
    }
    let bytes = storage
        .read(path)
        .await
        .map_err(|e| NoText::Failed(e.to_string()))?;
    document(bytes, name, mime_type, max_chars)
        .await
        .map(|document| document.text)
}

/// [`text_of`] a file of this machine.
pub async fn text_of_file(
    path: &Path,
    name: &str,
    mime_type: &str,
    max_chars: usize,
) -> Result<String, NoText> {
    let (storage, rel) = Storage::local_file(path).map_err(|e| NoText::Failed(e.to_string()))?;
    text_of(&storage, &rel, name, mime_type, max_chars).await
}

/// What is read from a document.
#[derive(Debug)]
pub struct Document {
    pub text: String,
    /// Its title, when it gives one (a web page's `<title>`, a PDF's).
    pub title: Option<String>,
}

/// The text and title of a document's bytes, the text at most `max_chars`
/// characters.
pub async fn document(
    bytes: Vec<u8>,
    name: &str,
    mime_type: &str,
    max_chars: usize,
) -> Result<Document, NoText> {
    let input = xberg::ExtractInput::from_bytes(bytes, mime_type, Some(name.to_string()));
    let config = xberg::ExtractionConfig {
        use_cache: false,
        ..Default::default()
    };
    // A task of its own: a panic in a parser is a failure of this file, not
    // of the job.
    let extraction = tokio::spawn(async move { xberg::extract(input, &config).await });
    let result = match tokio::time::timeout(TIMEOUT, extraction).await {
        Err(_) => return Err(NoText::Failed("it took too long".into())),
        Ok(Err(e)) => return Err(NoText::Failed(format!("the extractor failed: {e}"))),
        // A kind xberg has no extractor for has no text to us.
        Ok(Ok(Err(xberg::XbergError::UnsupportedFormat(_)))) => return Err(NoText::Empty),
        Ok(Ok(Err(e))) => return Err(NoText::Failed(e.to_string())),
        Ok(Ok(Ok(result))) => result,
    };
    let title = result
        .results
        .iter()
        .find_map(|document| document.metadata.title.clone())
        .map(|t| squeeze(&t).replace('\n', " "))
        .filter(|t| !t.is_empty());
    let text: String = result
        .results
        .iter()
        .map(|document| document.content.as_str())
        .collect::<Vec<_>>()
        .join("\n\n");
    let text = squeeze(&text);
    if text.is_empty() {
        return Err(NoText::Empty);
    }
    Ok(Document {
        text: truncate(&text, max_chars),
        title,
    })
}

/// An image found in a document: its bytes and type.
#[derive(Debug)]
pub struct PageImage {
    pub bytes: Vec<u8>,
    /// `image/jpeg`, `image/png`, ...
    pub mime_type: String,
}

/// The images xberg finds in a document (a scanned PDF's pages), in their
/// order, at most `max`; only those of a type a vision model takes (JPEG,
/// PNG, WebP, GIF).
pub async fn page_images(
    bytes: Vec<u8>,
    mime_type: &str,
    max: usize,
) -> Result<Vec<PageImage>, NoText> {
    let input = xberg::ExtractInput::from_bytes(bytes, mime_type, None);
    let config = xberg::ExtractionConfig {
        use_cache: false,
        images: Some(xberg::ImageExtractionConfig {
            extract_images: true,
            inject_placeholders: false,
            run_ocr_on_images: false,
            ..Default::default()
        }),
        ..Default::default()
    };
    let extraction = tokio::spawn(async move { xberg::extract(input, &config).await });
    let result = match tokio::time::timeout(TIMEOUT, extraction).await {
        Err(_) => return Err(NoText::Failed("it took too long".into())),
        Ok(Err(e)) => return Err(NoText::Failed(format!("the extractor failed: {e}"))),
        Ok(Ok(Err(xberg::XbergError::UnsupportedFormat(_)))) => return Err(NoText::Empty),
        Ok(Ok(Err(e))) => return Err(NoText::Failed(e.to_string())),
        Ok(Ok(Ok(result))) => result,
    };
    let images: Vec<PageImage> = result
        .results
        .into_iter()
        .flat_map(|document| document.images.unwrap_or_default())
        .filter(|image| !image.is_mask)
        .filter_map(|image| {
            let mime_type = match image.format.to_lowercase().as_str() {
                "jpeg" | "jpg" => "image/jpeg",
                "png" => "image/png",
                "webp" => "image/webp",
                "gif" => "image/gif",
                _ => return None,
            };
            Some(PageImage {
                bytes: image.data.to_vec(),
                mime_type: mime_type.into(),
            })
        })
        .take(max)
        .collect();
    if images.is_empty() {
        return Err(NoText::Empty);
    }
    Ok(images)
}

/// A document's text as Markdown (headings, lists, links kept), for the
/// Markdown copy of a page.
pub async fn markdown(bytes: Vec<u8>, mime_type: &str) -> Result<String, NoText> {
    let input = xberg::ExtractInput::from_bytes(bytes, mime_type, None);
    let config = xberg::ExtractionConfig {
        use_cache: false,
        output_format: xberg::OutputFormat::Markdown,
        ..Default::default()
    };
    let extraction = tokio::spawn(async move { xberg::extract(input, &config).await });
    match tokio::time::timeout(TIMEOUT, extraction).await {
        Err(_) => Err(NoText::Failed("it took too long".into())),
        Ok(Err(e)) => Err(NoText::Failed(format!("the extractor failed: {e}"))),
        Ok(Ok(Err(e))) => Err(NoText::Failed(e.to_string())),
        Ok(Ok(Ok(result))) => Ok(result
            .results
            .iter()
            .map(|d| d.content.trim())
            .collect::<Vec<_>>()
            .join("\n\n")),
    }
}

/// Trailing spaces off each line, runs of spaces to one, and at most one
/// blank line in a row.
pub fn squeeze(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut blank = false;
    for line in text.lines() {
        let line = line.split_whitespace().collect::<Vec<_>>().join(" ");
        if line.is_empty() {
            blank = !out.is_empty();
            continue;
        }
        if blank {
            out.push('\n');
            blank = false;
        }
        if !out.is_empty() {
            out.push('\n');
        }
        out.push_str(&line);
    }
    out
}

/// At most `max` characters, cut at a character boundary.
pub fn truncate(text: &str, max: usize) -> String {
    match text.char_indices().nth(max) {
        Some((at, _)) => text[..at].to_string(),
        None => text.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn text_is_squeezed_and_cut() {
        assert_eq!(
            squeeze("  Title  \n\n\n\nfirst   line\t \nsecond\n\n"),
            "Title\n\nfirst line\nsecond"
        );
        assert_eq!(truncate("ærlig", 3), "ærl");
        assert_eq!(truncate("ab", 3), "ab");
    }

    #[tokio::test]
    async fn plain_text_and_html_are_read() {
        let dir = tempfile_dir();
        let txt = dir.join("note.txt");
        std::fs::write(&txt, "Electricity bill\n\nAmount due: 120 EUR").unwrap();
        assert_eq!(
            text_of_file(&txt, "note.txt", "text/plain", MAX_CHARS)
                .await
                .unwrap(),
            "Electricity bill\n\nAmount due: 120 EUR"
        );
        let html = dir.join("page.html");
        std::fs::write(
            &html,
            "<html><head><title>T</title></head><body><h1>Lease</h1><p>Rent: 900</p></body></html>",
        )
        .unwrap();
        let text = text_of_file(&html, "page.html", "text/html", MAX_CHARS)
            .await
            .unwrap();
        assert!(
            text.contains("Lease") && text.contains("Rent: 900"),
            "{text}"
        );
        let empty = dir.join("empty.txt");
        std::fs::write(&empty, "  \n ").unwrap();
        assert_eq!(
            text_of_file(&empty, "empty.txt", "text/plain", MAX_CHARS).await,
            Err(NoText::Empty)
        );
    }

    #[tokio::test]
    async fn a_pdf_is_read() {
        let path =
            std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/policy.pdf");
        let text = text_of_file(&path, "policy.pdf", "application/pdf", MAX_CHARS)
            .await
            .unwrap();
        assert!(text.contains("Insurance policy renewal 2026"), "{text}");
    }

    #[tokio::test]
    async fn a_scans_pages_are_images() {
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/scan.pdf");
        assert_eq!(
            text_of_file(&path, "scan.pdf", "application/pdf", MAX_CHARS).await,
            Err(NoText::Empty)
        );
        let pages = page_images(std::fs::read(&path).unwrap(), "application/pdf", 20)
            .await
            .unwrap();
        assert_eq!(pages.len(), 1);
        assert!(
            pages[0].mime_type.starts_with("image/"),
            "{}",
            pages[0].mime_type
        );
        assert!(!pages[0].bytes.is_empty());
    }

    fn tempfile_dir() -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!("organizer-extract-{}", uuid::Uuid::now_v7()));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }
}

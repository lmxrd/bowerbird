//! The model's proposal for an item: the prompt, and the answer checked
//! against the tree before anything is done with it (docs/ARCHITECTURE.md
//! §5, "propose" and "validate").
//!
//! What goes into the prompt is exactly what the settings pages describe
//! under "What Organizer sends to the AI" (docs/M2_DESIGN.md §2.3): the
//! item's name, type, size and dates, the user's note, the folders shown
//! (`retrieve.rs`) with a few file names each, the text read from it when
//! that is allowed, and the user's recent corrections. Change one, change
//! the other.

use super::retrieve::Context;
use crate::files::names;
use crate::routes::drop_box::FolderFreedom;
use serde::Deserialize;
use uuid::Uuid;

/// The longest reason kept.
const MAX_REASON: usize = 500;

/// What the prompt says about an item.
pub struct ItemFacts<'a> {
    pub name: &'a str,
    pub mime_type: Option<&'a str>,
    pub size: Option<i64>,
    pub dropped_at: &'a str,
    pub note: Option<&'a str>,
    pub url: Option<&'a str>,
    pub text: Option<&'a str>,
}

/// A place the user chose over a proposal, as an example.
pub struct Example {
    pub summary: String,
    pub proposed: String,
    pub chosen: String,
}

/// The system prompt: the task, the rules of the user's settings, and the
/// form of the answer.
pub fn system_prompt(freedom: FolderFreedom, allowed_top: Option<&[String]>) -> String {
    let folders = match freedom {
        FolderFreedom::Consistent => {
            "Strongly prefer an existing folder. Propose one new folder only when no existing folder fits at all."
        }
        FolderFreedom::Balanced => {
            "Prefer an existing folder. Propose one new subfolder when it clearly groups this item with others like it."
        }
        FolderFreedom::Refined => {
            "Prefer specific places: propose up to two levels of new subfolders when they make the place clearly more specific."
        }
    };
    let top = match allowed_top {
        Some(top) => format!(
            "\nOnly these top-level folders may be used (or created): {}.",
            top.join(", ")
        ),
        None => String::new(),
    };
    format!(
        "You file things into a person's folder tree. Given one item, choose the folder it belongs in.
{folders}{top}
Keep the item's name unless it says nothing about its content (like \"scan_0042.pdf\" or \"IMG_1234.jpg\"); then propose a short, clear name in the language of the content, with the same extension.
Answer with JSON only, in this form:
{{\"folder\": \"<an existing folder's path, exactly as listed, or \\\"\\\" for the top>\", \"new_folders\": [\"<a new folder under it>\"], \"name\": <a new name, or null>, \"reason\": \"<one short sentence>\", \"confidence\": <0 to 1>}}"
    )
}

/// The message about the item: its facts, the tree, and the examples.
pub fn user_prompt(item: &ItemFacts, context: &Context, examples: &[Example]) -> String {
    let mut out = String::from("Item:\n");
    out.push_str(&format!("- name: {}\n", item.name));
    if let Some(mime) = item.mime_type {
        out.push_str(&format!("- type: {mime}\n"));
    }
    if let Some(size) = item.size {
        out.push_str(&format!("- size: {size} bytes\n"));
    }
    out.push_str(&format!(
        "- dropped: {}\n",
        &item.dropped_at[..10.min(item.dropped_at.len())]
    ));
    if let Some(url) = item.url {
        out.push_str(&format!("- link: {url}\n"));
    }
    if let Some(note) = item.note {
        out.push_str(&format!("- the person's note: {note}\n"));
    }
    if let Some(text) = item.text {
        out.push_str("- its text:\n\"\"\"\n");
        out.push_str(text);
        out.push_str("\n\"\"\"\n");
    }
    out.push_str("\nFolders (path, and a few files in it):\n");
    let outline = context.outline();
    if outline.is_empty() {
        out.push_str("(none yet)\n");
    } else {
        out.push_str(&outline);
    }
    if !examples.is_empty() {
        out.push_str("\nPlaces the person chose instead of earlier proposals:\n");
        for example in examples {
            out.push_str(&format!(
                "- {}: proposed {}, chosen {}\n",
                example.summary, example.proposed, example.chosen
            ));
        }
    }
    out
}

/// A proposal checked against the tree.
#[derive(Debug, Clone, PartialEq)]
pub struct Proposal {
    /// The existing folder; `None` for the top of the tree.
    pub folder_id: Option<Uuid>,
    pub folder_path: String,
    /// New folders under it, each under the one before.
    pub new_folders: Vec<String>,
    /// A new name; `None` keeps the item's.
    pub name: Option<String>,
    pub reason: String,
    pub confidence: f64,
}

impl Proposal {
    /// Where it puts the item: the folders and the name, as a path.
    pub fn path(&self, item_name: &str) -> String {
        let mut parts: Vec<&str> = Vec::new();
        if !self.folder_path.is_empty() {
            parts.push(&self.folder_path);
        }
        parts.extend(self.new_folders.iter().map(String::as_str));
        parts.push(self.name.as_deref().unwrap_or(item_name));
        parts.join("/")
    }
}

/// The answer as the model gives it.
#[derive(Deserialize)]
struct Answer {
    #[serde(default)]
    folder: Option<String>,
    #[serde(default)]
    new_folders: Option<Vec<String>>,
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    reason: Option<String>,
    #[serde(default)]
    confidence: Option<f64>,
}

/// The most new folders the setting allows.
pub fn max_new_folders(freedom: FolderFreedom) -> usize {
    match freedom {
        FolderFreedom::Consistent | FolderFreedom::Balanced => 1,
        FolderFreedom::Refined => 2,
    }
}

/// The model's answer, checked: the folder must be one it was shown, new
/// folders and the name must be valid names, within the setting's limits
/// and the allowed top-level folders; a new name keeps the extension. An
/// error says what was wrong, to tell the model on a second try.
pub fn parse(
    answer: &str,
    item_name: &str,
    context: &Context,
    freedom: FolderFreedom,
    allowed_top: Option<&[String]>,
) -> Result<Proposal, String> {
    let json = json_object(answer).ok_or("The answer is not a JSON object")?;
    let answer: Answer =
        serde_json::from_str(json).map_err(|e| format!("The JSON does not fit the form: {e}"))?;

    let folder = answer.folder.unwrap_or_default();
    let folder = folder.trim().trim_matches('/');
    let (folder_id, folder_path) = if folder.is_empty() {
        (None, String::new())
    } else {
        let found = context
            .find(folder)
            .ok_or_else(|| format!("The folder \"{folder}\" is not one of those listed"))?;
        (Some(found.id), found.display())
    };

    let mut new_folders = Vec::new();
    for raw in answer.new_folders.unwrap_or_default() {
        // A path given as one entry counts as its parts.
        for part in raw.split('/').map(str::trim).filter(|p| !p.is_empty()) {
            let name = names::new_name(part)
                .map_err(|e| format!("The new folder \"{part}\" is not a valid name: {e}"))?;
            new_folders.push(name);
        }
    }
    let limit = max_new_folders(freedom);
    if new_folders.len() > limit {
        return Err(format!(
            "At most {limit} new folder(s) may be proposed, not {}",
            new_folders.len()
        ));
    }
    if folder_id.is_none() && new_folders.is_empty() {
        return Err("An item cannot go at the top: choose a folder, or a new one".into());
    }
    // A new top-level folder must be one of those allowed.
    if let (None, Some(top)) = (folder_id, allowed_top) {
        let first = names::fold(&new_folders[0]);
        if !top.iter().any(|t| names::fold(t) == first) {
            return Err(format!(
                "The top-level folder \"{}\" is not allowed",
                new_folders[0]
            ));
        }
    }

    let name = match answer.name.map(|n| n.trim().to_string()) {
        None => None,
        Some(n) if n.is_empty() || n == item_name => None,
        Some(n) => {
            let n = with_extension_of(&n, item_name);
            Some(
                names::new_name(&n)
                    .map_err(|e| format!("The name \"{n}\" is not a valid name: {e}"))?,
            )
        }
    };
    let reason = super::extract::truncate(answer.reason.unwrap_or_default().trim(), MAX_REASON);
    let confidence = answer.confidence.unwrap_or(0.5);
    let confidence = if confidence.is_finite() {
        confidence.clamp(0.0, 1.0)
    } else {
        0.5
    };
    Ok(Proposal {
        folder_id,
        folder_path,
        new_folders,
        name,
        reason,
        confidence,
    })
}

/// The first JSON object in a text: models wrap it in prose or a code
/// block now and then.
fn json_object(text: &str) -> Option<&str> {
    let start = text.find('{')?;
    let end = text.rfind('}')?;
    (end > start).then(|| &text[start..=end])
}

/// A new name with the original's extension: added when it is missing or
/// another (a model that renames `x.pdf` to `Invoice` means `Invoice.pdf`).
pub fn with_extension_of(name: &str, original: &str) -> String {
    let extension = |n: &str| {
        std::path::Path::new(n)
            .extension()
            .map(|e| e.to_string_lossy().to_lowercase())
    };
    match extension(original) {
        Some(ext) if extension(name).as_deref() != Some(ext.as_str()) => {
            let stem = match extension(name) {
                // Another known extension: replaced.
                Some(other)
                    if other.len() <= 5 && mime_guess::from_ext(&other).first().is_some() =>
                {
                    &name[..name.len() - other.len() - 1]
                }
                _ => name,
            };
            let original_ext = std::path::Path::new(original)
                .extension()
                .map(|e| e.to_string_lossy().into_owned())
                .unwrap_or(ext);
            format!("{stem}.{original_ext}")
        }
        _ => name.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::drop_box::retrieve::{Folder, context};

    fn tree() -> Context {
        let folder = |path: &str| Folder {
            id: Uuid::now_v7(),
            path: path.split('/').map(String::from).collect(),
            examples: vec![],
        };
        context(
            vec![folder("Home"), folder("Home/Bills"), folder("Health")],
            &[],
            None,
        )
    }

    #[test]
    fn a_valid_answer_is_kept_and_cleaned() {
        let context = tree();
        let answer = "Sure! ```json\n{\"folder\": \"home/bills/\", \"new_folders\": [\"Power\"], \"name\": \"Power bill January\", \"reason\": \"An electricity invoice.\", \"confidence\": 1.4}\n```";
        let p = parse(
            answer,
            "scan_0042.pdf",
            &context,
            FolderFreedom::Balanced,
            None,
        )
        .unwrap();
        assert_eq!(p.folder_id, Some(context.find("Home/Bills").unwrap().id));
        assert_eq!(p.folder_path, "Home/Bills");
        assert_eq!(p.new_folders, ["Power"]);
        assert_eq!(p.name.as_deref(), Some("Power bill January.pdf"));
        assert_eq!(p.confidence, 1.0);
        assert_eq!(
            p.path("scan_0042.pdf"),
            "Home/Bills/Power/Power bill January.pdf"
        );

        // The same name, or none, keeps the item's own.
        let p = parse(
            r#"{"folder": "Health", "name": "x.pdf", "reason": "r", "confidence": 0.7}"#,
            "x.pdf",
            &context,
            FolderFreedom::Consistent,
            None,
        )
        .unwrap();
        assert_eq!((p.name, p.new_folders.len()), (None, 0));
    }

    #[test]
    fn answers_that_do_not_fit_the_tree_are_refused() {
        let context = tree();
        let refused = |answer: &str, freedom, top: Option<&[String]>| {
            parse(answer, "a.pdf", &context, freedom, top).unwrap_err()
        };
        assert!(refused("no idea", FolderFreedom::Balanced, None).contains("not a JSON"));
        assert!(
            refused(r#"{"folder": "Work"}"#, FolderFreedom::Balanced, None)
                .contains("not one of those listed")
        );
        assert!(
            refused(
                r#"{"folder": "Home", "new_folders": ["A", "B"]}"#,
                FolderFreedom::Balanced,
                None
            )
            .contains("At most 1")
        );
        assert!(
            parse(
                r#"{"folder": "Home", "new_folders": ["A/B"]}"#,
                "a.pdf",
                &context,
                FolderFreedom::Refined,
                None
            )
            .is_ok()
        );
        assert!(
            refused(
                r#"{"folder": "Home", "new_folders": [".."]}"#,
                FolderFreedom::Refined,
                None
            )
            .contains("not a valid name")
        );
        assert!(refused(r#"{"folder": ""}"#, FolderFreedom::Balanced, None).contains("at the top"));
        let top = ["Home".to_string()];
        assert!(
            refused(
                r#"{"folder": "", "new_folders": ["Work"]}"#,
                FolderFreedom::Balanced,
                Some(&top)
            )
            .contains("not allowed")
        );
    }

    #[test]
    fn a_new_name_keeps_the_extension() {
        assert_eq!(with_extension_of("Invoice", "scan.pdf"), "Invoice.pdf");
        assert_eq!(with_extension_of("Invoice.PDF", "scan.pdf"), "Invoice.PDF");
        assert_eq!(with_extension_of("Invoice.docx", "scan.pdf"), "Invoice.pdf");
        assert_eq!(with_extension_of("Report v1.2", "a.pdf"), "Report v1.2.pdf");
        assert_eq!(with_extension_of("Notes", "README"), "Notes");
    }

    #[test]
    fn the_prompt_holds_what_the_settings_say_is_sent() {
        let context = tree();
        let item = ItemFacts {
            name: "scan.pdf",
            mime_type: Some("application/pdf"),
            size: Some(1200),
            dropped_at: "2026-11-06T10:00:00.000Z",
            note: Some("power bill"),
            url: None,
            text: Some("Amount due: 120"),
        };
        let examples = [Example {
            summary: "x.pdf".into(),
            proposed: "Home/x.pdf".into(),
            chosen: "Health/x.pdf".into(),
        }];
        let prompt = user_prompt(&item, &context, &examples);
        for part in [
            "- name: scan.pdf",
            "- type: application/pdf",
            "- size: 1200 bytes",
            "- dropped: 2026-11-06",
            "- the person's note: power bill",
            "Amount due: 120",
            "Home/Bills\n",
            "proposed Home/x.pdf, chosen Health/x.pdf",
        ] {
            assert!(prompt.contains(part), "{part} in {prompt}");
        }
        let system = system_prompt(FolderFreedom::Refined, Some(&["Home".to_string()]));
        assert!(
            system.contains("two levels")
                && system.contains("Only these top-level folders may be used (or created): Home.")
        );
    }
}

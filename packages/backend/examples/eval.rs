//! The drop box's eval harness (`mise run eval`, manual only): the samples
//! of `tests/eval/` proposed by the model of the `AI_*` variables, through
//! the same retrieval, prompt and checks as the server, and scored.
//!
//! `--freedom consistent|balanced|refined` runs one setting (default all),
//! `--limit N` the first N samples.

use organizer::ai::{Ai, Choices, Env, Registry, Work};
use organizer::drop_box::propose::{self, ItemFacts};
use organizer::drop_box::retrieve::{self, Folder};
use organizer::routes::drop_box::FolderFreedom;
use std::sync::Arc;
use uuid::Uuid;

#[derive(serde::Deserialize, Clone)]
struct Sample {
    name: String,
    note: Option<String>,
    text: Option<String>,
    expected: String,
}

#[derive(Default)]
struct Score {
    right: usize,
    close: usize,
    wrong: usize,
    /// No usable answer, twice.
    unusable: usize,
    /// The model failed.
    failed: usize,
}

enum Outcome {
    Right,
    Close,
    Wrong(String),
    Unusable(String),
    Failed(String),
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let arg = |name: &str| {
        args.iter()
            .position(|a| a == name)
            .and_then(|i| args.get(i + 1).cloned())
    };
    let freedoms = match arg("--freedom").as_deref() {
        None => vec![
            FolderFreedom::Consistent,
            FolderFreedom::Balanced,
            FolderFreedom::Refined,
        ],
        Some("consistent") => vec![FolderFreedom::Consistent],
        Some("balanced") => vec![FolderFreedom::Balanced],
        Some("refined") => vec![FolderFreedom::Refined],
        Some(other) => anyhow::bail!("--freedom is consistent, balanced or refined, not {other}"),
    };
    let limit: usize = arg("--limit")
        .map(|l| l.parse())
        .transpose()?
        .unwrap_or(usize::MAX);

    let env = Env::from_env().map_err(anyhow::Error::msg)?;
    let registry = Registry::new(&env, Vec::new(), Choices::default());
    let Some(model) = registry.model_for(Work::Proposals).cloned() else {
        anyhow::bail!("set AI_PROVIDER and AI_MODEL (and AI_ENDPOINT, AI_API_KEY as needed)");
    };
    println!(
        "model {} ({}, {}), concurrency {}",
        model.model,
        model.provider,
        if model.endpoint.is_empty() {
            "its own endpoint"
        } else {
            &model.endpoint
        },
        registry.choices.concurrency
    );
    let ai = Arc::new(Ai::new(registry, env));

    let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/eval");
    let folders: Vec<Folder> = std::fs::read_to_string(dir.join("tree.txt"))?
        .lines()
        .filter(|l| !l.trim().is_empty())
        .map(|line| {
            let (path, examples) = line.split_once(" | ").unwrap_or((line, ""));
            Folder {
                id: Uuid::now_v7(),
                path: path.split('/').map(String::from).collect(),
                examples: examples
                    .split(", ")
                    .filter(|e| !e.is_empty())
                    .map(String::from)
                    .collect(),
            }
        })
        .collect();
    let samples: Vec<Sample> = std::fs::read_to_string(dir.join("samples.jsonl"))?
        .lines()
        .filter(|l| !l.trim().is_empty())
        .map(serde_json::from_str)
        .collect::<Result<_, _>>()?;
    let samples: Vec<Sample> = samples.into_iter().take(limit).collect();

    for freedom in freedoms {
        let started = std::time::Instant::now();
        let mut tasks = tokio::task::JoinSet::new();
        for (i, sample) in samples.iter().cloned().enumerate() {
            let (ai, folders) = (ai.clone(), folders.clone());
            tasks.spawn(
                async move { (i, sample.clone(), run(&ai, folders, &sample, freedom).await) },
            );
        }
        let mut score = Score::default();
        let mut results = Vec::new();
        while let Some(result) = tasks.join_next().await {
            results.push(result?);
        }
        results.sort_by_key(|(i, _, _)| *i);
        for (_, sample, outcome) in results {
            match outcome {
                Outcome::Right => score.right += 1,
                Outcome::Close => score.close += 1,
                Outcome::Wrong(got) => {
                    score.wrong += 1;
                    println!(
                        "  wrong: {} -> {got} (expected {})",
                        sample.name, sample.expected
                    );
                }
                Outcome::Unusable(why) => {
                    score.unusable += 1;
                    println!("  unusable: {}: {why}", sample.name);
                }
                Outcome::Failed(why) => {
                    score.failed += 1;
                    println!("  failed: {}: {why}", sample.name);
                }
            }
        }
        let n = samples.len().max(1) as f64;
        println!(
            "{freedom:?}: {:.1} % right, {:.1} % close, {} wrong, {} unusable, {} failed, of {} in {:.0} s",
            100.0 * score.right as f64 / n,
            100.0 * score.close as f64 / n,
            score.wrong,
            score.unusable,
            score.failed,
            samples.len(),
            started.elapsed().as_secs_f64()
        );
    }
    Ok(())
}

/// One sample, as the analysis would do it.
async fn run(ai: &Ai, folders: Vec<Folder>, sample: &Sample, freedom: FolderFreedom) -> Outcome {
    let words = retrieve::item_words(&sample.name, sample.note.as_deref(), sample.text.as_deref());
    let context = retrieve::context(folders, &words, None);
    let mime = mime_guess::from_path(&sample.name)
        .first_or_octet_stream()
        .to_string();
    let facts = ItemFacts {
        name: &sample.name,
        mime_type: Some(&mime),
        size: None,
        dropped_at: "2026-11-06",
        note: sample.note.as_deref(),
        url: None,
        text: sample.text.as_deref(),
    };
    let system = propose::system_prompt(freedom, None);
    let mut prompt = propose::user_prompt(&facts, &context, &[]);
    let mut problem = String::new();
    for _ in 0..2 {
        let answer = match ai.chat(None, &system, &prompt, true).await {
            Ok(answer) => answer,
            Err(e) => return Outcome::Failed(e.to_string()),
        };
        match propose::parse(&answer, &sample.name, &context, freedom, None) {
            Ok(p) => {
                let mut parts: Vec<String> = Vec::new();
                if !p.folder_path.is_empty() {
                    parts.push(p.folder_path.clone());
                }
                parts.extend(p.new_folders.iter().cloned());
                let got = parts.join("/");
                let (got_l, expected_l) = (got.to_lowercase(), sample.expected.to_lowercase());
                return if got_l == expected_l {
                    Outcome::Right
                } else if expected_l.starts_with(&format!("{got_l}/"))
                    || got_l.starts_with(&format!("{expected_l}/"))
                {
                    Outcome::Close
                } else {
                    Outcome::Wrong(got)
                };
            }
            Err(e) => {
                prompt.push_str(&format!(
                    "\nYour previous answer could not be used: {e}. Answer again with JSON only.\n"
                ));
                problem = e;
            }
        }
    }
    Outcome::Unusable(problem)
}

//! The AI models Organizer uses, through the `genai` crate: any provider it
//! has an adapter for (an OpenAI-compatible server such as Ollama,
//! llama.cpp, LM Studio or vLLM, Ollama's own API, Anthropic, Gemini, ...),
//! and an OpenAI-compatible `/audio/transcriptions` endpoint for speech.
//!
//! A registry (docs/M6C_DESIGN.md §1): the models a superadmin registers (the
//! `ai_models` rows), each with what it can do (chat, vision, speech,
//! embeddings); a default model (a chat model); and the model chosen for
//! each kind of work ([`Work`]): the drop box's proposals (the default
//! unless another is chosen), embeddings, photos and images, scanned PDFs
//! and audio (off until one is chosen). The AI_* variables are one more
//! model, "from the environment", locked and used as the default (and
//! `AI_EMBEDDINGS_MODEL` a locked embeddings model); `AI_MODEL_IMAGES`,
//! `AI_MODEL_SCANS`, `AI_MODEL_AUDIO` and `AI_MODEL_EMBEDDINGS` lock a
//! work's model by its registered name. All of it is kept here, in memory,
//! and replaced when a superadmin saves: no restart.
//!
//! Every request goes out on our own reqwest (ring for TLS, see `lib.rs`),
//! with the endpoint and key of its model: genai's own defaults, which read
//! keys from variables such as `OPENAI_API_KEY`, are never used. Every
//! request, whatever its model, waits for one of the `AI_CONCURRENCY`
//! permits.

use crate::db::Db;
use genai::adapter::AdapterKind;
use genai::chat::{
    Binary, ChatMessage, ChatOptions, ChatRequest, ChatResponseFormat, ContentPart, MessageContent,
};
use genai::resolver::{AuthData, Endpoint};
use genai::{ModelIden, ServiceTarget};
use serde::{Deserialize, Serialize};
use std::sync::{Arc, RwLock};
use std::time::Duration;
use tokio::sync::Semaphore;
use utoipa::ToSchema;
use uuid::Uuid;

/// The most requests at once that `AI_CONCURRENCY` may ask for.
pub const MAX_CONCURRENCY: i64 = 16;
/// The longest `AI_TIMEOUT`, in seconds: a slow local model on a large
/// document can take minutes, but not an hour.
pub const MAX_TIMEOUT: i64 = 1800;
/// The id of the model of the AI_* variables.
pub const ENV_MODEL: &str = "env";
/// The id of the embeddings model of `AI_EMBEDDINGS_MODEL`.
pub const ENV_EMBEDDINGS: &str = "env-embeddings";
/// The longest name of a registered model.
pub const MAX_NAME: usize = 80;

/// What a model can do.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum Capability {
    /// Answer a prompt: the drop box's proposals.
    Chat,
    /// Read an image: photos and scanned pages.
    Vision,
    /// Transcribe a recording (an OpenAI-compatible `/audio/transcriptions`).
    Speech,
    /// Embed text: semantic search and retrieval.
    Embeddings,
}

/// A kind of work, and the model chosen for it.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum Work {
    /// The drop box's proposals: the default model unless another is chosen.
    Proposals,
    /// Semantic search and better proposals.
    Embeddings,
    /// Photos and images, read by a vision model.
    Images,
    /// Scanned PDFs (pages without text), read by a vision model.
    Scans,
    /// Recordings, transcribed by a speech model.
    Audio,
}

impl Work {
    pub const ALL: [Work; 5] = [
        Work::Proposals,
        Work::Embeddings,
        Work::Images,
        Work::Scans,
        Work::Audio,
    ];

    /// What its model must be able to do.
    pub fn needs(self) -> Capability {
        match self {
            Work::Proposals => Capability::Chat,
            Work::Embeddings => Capability::Embeddings,
            Work::Images | Work::Scans => Capability::Vision,
            Work::Audio => Capability::Speech,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Work::Proposals => "proposals",
            Work::Embeddings => "embeddings",
            Work::Images => "images",
            Work::Scans => "scans",
            Work::Audio => "audio",
        }
    }

    /// Its column in `ai_settings`.
    fn column(self) -> &'static str {
        match self {
            Work::Proposals => "proposals_model",
            Work::Embeddings => "embeddings_model_id",
            Work::Images => "images_model",
            Work::Scans => "scans_model",
            Work::Audio => "audio_model",
        }
    }
}

/// A model, registered or from the environment. An empty string is a
/// field that is not set.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Model {
    /// A registered model's UUID, or [`ENV_MODEL`] / [`ENV_EMBEDDINGS`].
    pub id: String,
    pub name: String,
    pub provider: String,
    pub endpoint: String,
    pub api_key: String,
    /// The provider's name for the model.
    pub model: String,
    pub chat: bool,
    pub vision: bool,
    pub speech: bool,
    pub embeddings: bool,
}

impl Model {
    pub fn can(&self, capability: Capability) -> bool {
        match capability {
            Capability::Chat => self.chat,
            Capability::Vision => self.vision,
            Capability::Speech => self.speech,
            Capability::Embeddings => self.embeddings,
        }
    }

    pub fn capabilities(&self) -> Vec<Capability> {
        [
            Capability::Chat,
            Capability::Vision,
            Capability::Speech,
            Capability::Embeddings,
        ]
        .into_iter()
        .filter(|c| self.can(*c))
        .collect()
    }

    pub fn set_capabilities(&mut self, capabilities: &[Capability]) {
        self.chat = capabilities.contains(&Capability::Chat);
        self.vision = capabilities.contains(&Capability::Vision);
        self.speech = capabilities.contains(&Capability::Speech);
        self.embeddings = capabilities.contains(&Capability::Embeddings);
    }

    /// From the AI_* variables, not the superadmin's to change.
    pub fn from_env(&self) -> bool {
        self.id == ENV_MODEL || self.id == ENV_EMBEDDINGS
    }

    /// The model's own mistakes, before anything is asked of the provider.
    pub fn validate(&self) -> Result<(), String> {
        let name = self.name.trim();
        if name.is_empty() {
            return Err("Give the model a name".into());
        }
        if name.chars().count() > MAX_NAME {
            return Err(format!("The name is longer than {MAX_NAME} characters"));
        }
        if self.adapter().is_none() {
            return Err(format!(
                "The provider must be one of {}, not `{}`",
                providers().join(", "),
                self.provider
            ));
        }
        if self.model.trim().is_empty() {
            return Err("Give the provider's name of the model".into());
        }
        if !self.endpoint.is_empty()
            && !(self.endpoint.starts_with("http://") || self.endpoint.starts_with("https://"))
        {
            return Err("The endpoint must be an http:// or https:// URL".into());
        }
        if self.capabilities().is_empty() {
            return Err("Say what the model can do: chat, vision, speech or embeddings".into());
        }
        if self.speech && self.adapter() != Some(AdapterKind::OpenAI) {
            return Err(
                "Speech needs an OpenAI-compatible endpoint (its /audio/transcriptions)".into(),
            );
        }
        Ok(())
    }

    fn adapter(&self) -> Option<AdapterKind> {
        AdapterKind::from_lower_str(&self.provider)
    }
}

/// The providers genai has an adapter for, by the name the settings take.
pub fn providers() -> Vec<&'static str> {
    AdapterKind::all()
        .iter()
        .map(AdapterKind::as_lower_str)
        .collect()
}

/// The superadmin's choices, as stored: model ids, `None` for the default
/// (proposals) or off (the rest).
#[derive(Clone, Debug, PartialEq)]
pub struct Choices {
    pub default_model: Option<String>,
    pub proposals: Option<String>,
    pub embeddings: Option<String>,
    pub images: Option<String>,
    pub scans: Option<String>,
    pub audio: Option<String>,
    pub concurrency: i64,
    pub timeout_seconds: i64,
}

impl Default for Choices {
    fn default() -> Self {
        Choices {
            default_model: None,
            proposals: None,
            embeddings: None,
            images: None,
            scans: None,
            audio: None,
            concurrency: 1,
            timeout_seconds: 120,
        }
    }
}

impl Choices {
    pub fn get(&self, work: Work) -> Option<&String> {
        match work {
            Work::Proposals => self.proposals.as_ref(),
            Work::Embeddings => self.embeddings.as_ref(),
            Work::Images => self.images.as_ref(),
            Work::Scans => self.scans.as_ref(),
            Work::Audio => self.audio.as_ref(),
        }
    }

    pub fn set(&mut self, work: Work, model: Option<String>) {
        let slot = match work {
            Work::Proposals => &mut self.proposals,
            Work::Embeddings => &mut self.embeddings,
            Work::Images => &mut self.images,
            Work::Scans => &mut self.scans,
            Work::Audio => &mut self.audio,
        };
        *slot = model;
    }

    pub fn validate(&self) -> Result<(), String> {
        if !(1..=MAX_CONCURRENCY).contains(&self.concurrency) {
            return Err(format!(
                "The concurrency must be from 1 to {MAX_CONCURRENCY}"
            ));
        }
        if !(1..=MAX_TIMEOUT).contains(&self.timeout_seconds) {
            return Err(format!(
                "The timeout must be from 1 to {MAX_TIMEOUT} seconds"
            ));
        }
        Ok(())
    }
}

/// The AI_* environment variables.
#[derive(Clone, Default)]
pub struct Env {
    pub provider: Option<String>,
    pub endpoint: Option<String>,
    pub api_key: Option<String>,
    pub model: Option<String>,
    pub embeddings_model: Option<String>,
    pub embeddings_endpoint: Option<String>,
    pub concurrency: Option<i64>,
    pub timeout_seconds: Option<i64>,
    /// `AI_MODEL_IMAGES`: the registered name of the images' model.
    pub images: Option<String>,
    /// `AI_MODEL_SCANS`.
    pub scans: Option<String>,
    /// `AI_MODEL_AUDIO`.
    pub audio: Option<String>,
    /// `AI_MODEL_EMBEDDINGS`.
    pub embeddings: Option<String>,
}

impl Env {
    /// The AI_* variables. A number that is not one, or a model that does
    /// not make sense, is an error, which stops the start rather than being
    /// quietly ignored.
    pub fn from_env() -> Result<Self, String> {
        use crate::config::var;
        let number = |name: &str| -> Result<Option<i64>, String> {
            var(name)
                .map(|v| {
                    v.parse()
                        .map_err(|_| format!("{name} must be a whole number, not `{v}`"))
                })
                .transpose()
        };
        let env = Env {
            provider: var("AI_PROVIDER").map(|p| p.to_lowercase()),
            endpoint: var("AI_ENDPOINT"),
            api_key: var("AI_API_KEY"),
            model: var("AI_MODEL"),
            embeddings_model: var("AI_EMBEDDINGS_MODEL"),
            embeddings_endpoint: var("AI_EMBEDDINGS_ENDPOINT"),
            concurrency: number("AI_CONCURRENCY")?,
            timeout_seconds: number("AI_TIMEOUT")?,
            images: var("AI_MODEL_IMAGES"),
            scans: var("AI_MODEL_SCANS"),
            audio: var("AI_MODEL_AUDIO"),
            embeddings: var("AI_MODEL_EMBEDDINGS"),
        };
        env.validate()
            .map_err(|e| format!("The AI_* variables: {e}"))?;
        Ok(env)
    }

    /// The variables alone must make sense.
    pub fn validate(&self) -> Result<(), String> {
        let connection = [
            &self.provider,
            &self.endpoint,
            &self.api_key,
            &self.model,
            &self.embeddings_model,
            &self.embeddings_endpoint,
        ];
        if connection.iter().any(|v| v.is_some()) {
            if self.provider.is_none() {
                return Err("set AI_PROVIDER for the model of the AI_* variables".into());
            }
            if self.model.is_none() && self.embeddings_model.is_none() {
                return Err("set AI_MODEL (or AI_EMBEDDINGS_MODEL) with AI_PROVIDER".into());
            }
        }
        if self.embeddings_model.is_some() && self.embeddings.is_some() {
            return Err("AI_EMBEDDINGS_MODEL and AI_MODEL_EMBEDDINGS both choose the embeddings model: set one".into());
        }
        for model in self.models() {
            model.validate()?;
        }
        Choices {
            concurrency: self.concurrency.unwrap_or(1),
            timeout_seconds: self.timeout_seconds.unwrap_or(120),
            ..Choices::default()
        }
        .validate()
    }

    /// The models the variables define: the chat model of `AI_MODEL`, and
    /// the embeddings model of `AI_EMBEDDINGS_MODEL`.
    pub fn models(&self) -> Vec<Model> {
        let Some(provider) = &self.provider else {
            return Vec::new();
        };
        let mut models = Vec::new();
        let endpoint = self.endpoint.clone().unwrap_or_default();
        let api_key = self.api_key.clone().unwrap_or_default();
        if let Some(model) = &self.model {
            models.push(Model {
                id: ENV_MODEL.into(),
                name: "From the environment".into(),
                provider: provider.clone(),
                endpoint: endpoint.clone(),
                api_key: api_key.clone(),
                model: model.clone(),
                chat: true,
                ..Model::default()
            });
        }
        if let Some(model) = &self.embeddings_model {
            models.push(Model {
                id: ENV_EMBEDDINGS.into(),
                name: "From the environment (embeddings)".into(),
                provider: provider.clone(),
                endpoint: self.embeddings_endpoint.clone().unwrap_or(endpoint),
                api_key,
                model: model.clone(),
                embeddings: true,
                ..Model::default()
            });
        }
        models
    }

    /// The variables that make the environment's models, for the settings
    /// page.
    pub fn model_variables(&self) -> Vec<&'static str> {
        [
            ("AI_PROVIDER", self.provider.is_some()),
            ("AI_ENDPOINT", self.endpoint.is_some()),
            ("AI_API_KEY", self.api_key.is_some()),
            ("AI_MODEL", self.model.is_some()),
            ("AI_EMBEDDINGS_MODEL", self.embeddings_model.is_some()),
            ("AI_EMBEDDINGS_ENDPOINT", self.embeddings_endpoint.is_some()),
        ]
        .into_iter()
        .filter(|(_, set)| *set)
        .map(|(name, _)| name)
        .collect()
    }

    /// The registered name a variable locks a work's model to; for
    /// embeddings, `AI_EMBEDDINGS_MODEL`'s own model is by its id instead.
    fn work_name(&self, work: Work) -> Option<&String> {
        match work {
            Work::Proposals => None,
            Work::Embeddings => self.embeddings.as_ref(),
            Work::Images => self.images.as_ref(),
            Work::Scans => self.scans.as_ref(),
            Work::Audio => self.audio.as_ref(),
        }
    }

    /// The choices set by a variable, by the key the settings use
    /// (`default`, a work, `concurrency`, `timeout_seconds`), each with its
    /// variable.
    pub fn locked(&self) -> Vec<(&'static str, &'static str)> {
        [
            ("default", "AI_MODEL", self.model.is_some()),
            (
                "embeddings",
                if self.embeddings_model.is_some() {
                    "AI_EMBEDDINGS_MODEL"
                } else {
                    "AI_MODEL_EMBEDDINGS"
                },
                self.embeddings_model.is_some() || self.embeddings.is_some(),
            ),
            ("images", "AI_MODEL_IMAGES", self.images.is_some()),
            ("scans", "AI_MODEL_SCANS", self.scans.is_some()),
            ("audio", "AI_MODEL_AUDIO", self.audio.is_some()),
            ("concurrency", "AI_CONCURRENCY", self.concurrency.is_some()),
            (
                "timeout_seconds",
                "AI_TIMEOUT",
                self.timeout_seconds.is_some(),
            ),
        ]
        .into_iter()
        .filter(|(_, _, set)| *set)
        .map(|(key, variable, _)| (key, variable))
        .collect()
    }

    /// The variable that locks a key, if one does.
    pub fn lock(&self, key: &str) -> Option<&'static str> {
        self.locked()
            .into_iter()
            .find(|(k, _)| *k == key)
            .map(|(_, v)| v)
    }
}

/// The registry in effect: the models (the environment's first, then the
/// registered ones by name) and the choices, with the variables on top.
#[derive(Clone, Debug, Default)]
pub struct Registry {
    pub models: Vec<Model>,
    /// The stored choices, with `AI_CONCURRENCY` and `AI_TIMEOUT` on top.
    pub choices: Choices,
    /// The model of the work a variable locks, by work: its name, as the
    /// variable gives it.
    locked_names: Vec<(Work, String)>,
    env_default: bool,
    env_embeddings: bool,
}

impl Registry {
    pub fn new(env: &Env, stored: Vec<Model>, mut choices: Choices) -> Self {
        let mut models = env.models();
        models.extend(stored);
        if let Some(c) = env.concurrency {
            choices.concurrency = c;
        }
        if let Some(t) = env.timeout_seconds {
            choices.timeout_seconds = t;
        }
        Registry {
            models,
            choices,
            locked_names: Work::ALL
                .iter()
                .filter_map(|w| env.work_name(*w).map(|n| (*w, n.clone())))
                .collect(),
            env_default: env.model.is_some(),
            env_embeddings: env.embeddings_model.is_some(),
        }
    }

    pub fn model(&self, id: &str) -> Option<&Model> {
        self.models.iter().find(|m| m.id == id)
    }

    pub fn by_name(&self, name: &str) -> Option<&Model> {
        self.models.iter().find(|m| m.name == name)
    }

    /// The default model: the environment's, else the chosen one (a chat
    /// model).
    pub fn default_model(&self) -> Option<&Model> {
        if self.env_default {
            return self.model(ENV_MODEL);
        }
        self.choices
            .default_model
            .as_deref()
            .and_then(|id| self.model(id))
            .filter(|m| m.chat)
    }

    /// The model that does a kind of work; `None` when it is off (or, for
    /// proposals, when there is no default either).
    pub fn model_for(&self, work: Work) -> Option<&Model> {
        let chosen = if work == Work::Embeddings && self.env_embeddings {
            self.model(ENV_EMBEDDINGS)
        } else if let Some((_, name)) = self.locked_names.iter().find(|(w, _)| *w == work) {
            self.by_name(name)
        } else {
            self.choices.get(work).and_then(|id| self.model(id))
        };
        match chosen.filter(|m| m.can(work.needs())) {
            Some(model) => Some(model),
            None if work == Work::Proposals => self.default_model(),
            None => None,
        }
    }

    /// The model of a work for a user who chose one (the `ai.model.*`
    /// settings, docs/M7B_DESIGN.md §4): theirs when it is registered and
    /// can do the work, else [`Registry::model_for`].
    pub fn model_chosen(&self, work: Work, chosen: Option<&str>) -> Option<&Model> {
        chosen
            .and_then(|id| self.model(id))
            .filter(|m| m.can(work.needs()))
            .or_else(|| self.model_for(work))
    }

    /// The model a work uses as the settings page shows it: the one chosen
    /// (by the superadmin or a variable), `None` for the default or off.
    pub fn chosen(&self, work: Work) -> Option<String> {
        if work == Work::Proposals {
            return self
                .choices
                .proposals
                .clone()
                .filter(|id| self.model(id).is_some_and(|m| m.chat));
        }
        self.model_for(work).map(|m| m.id.clone())
    }
}

#[derive(sqlx::FromRow)]
struct ModelRow {
    id: Uuid,
    name: String,
    provider: String,
    endpoint: String,
    api_key: String,
    model: String,
    chat: bool,
    vision: bool,
    speech: bool,
    embeddings: bool,
}

impl From<ModelRow> for Model {
    fn from(r: ModelRow) -> Self {
        Model {
            id: r.id.to_string(),
            name: r.name,
            provider: r.provider,
            endpoint: r.endpoint,
            api_key: r.api_key,
            model: r.model,
            chat: r.chat,
            vision: r.vision,
            speech: r.speech,
            embeddings: r.embeddings,
        }
    }
}

/// The superadmin's registered models, by name.
pub async fn stored_models(db: &Db) -> Result<Vec<Model>, sqlx::Error> {
    let rows: Vec<ModelRow> = sqlx::query_as(
        "SELECT id, name, provider, endpoint, api_key, model, chat, vision, speech, embeddings
         FROM ai_models WHERE owner_id IS NULL ORDER BY name",
    )
    .fetch_all(db.reader())
    .await?;
    Ok(rows.into_iter().map(Model::from).collect())
}

/// The superadmin's stored choices.
pub async fn stored_choices(db: &Db) -> Result<Choices, sqlx::Error> {
    #[allow(clippy::type_complexity)]
    let row: (
        Option<Uuid>,
        Option<Uuid>,
        Option<Uuid>,
        Option<Uuid>,
        Option<Uuid>,
        Option<Uuid>,
        i64,
        i64,
    ) = sqlx::query_as(
        "SELECT default_model, proposals_model, embeddings_model_id, images_model, scans_model,
           audio_model, concurrency, timeout_seconds
         FROM ai_settings WHERE id = 1",
    )
    .fetch_one(db.reader())
    .await?;
    let id = |u: Option<Uuid>| u.map(|u| u.to_string());
    Ok(Choices {
        default_model: id(row.0),
        proposals: id(row.1),
        embeddings: id(row.2),
        images: id(row.3),
        scans: id(row.4),
        audio: id(row.5),
        concurrency: row.6,
        timeout_seconds: row.7,
    })
}

/// Store the superadmin's choices (ids of registered models; another id, such
/// as the environment's, is stored as none).
pub async fn store_choices(db: &Db, choices: &Choices) -> Result<(), sqlx::Error> {
    let id = |s: Option<&String>| s.and_then(|s| Uuid::parse_str(s).ok());
    let mut query = String::from(
        "UPDATE ai_settings SET default_model = ?1, concurrency = ?2, timeout_seconds = ?3",
    );
    for (n, work) in Work::ALL.iter().enumerate() {
        query.push_str(&format!(", {} = ?{}", work.column(), n + 4));
    }
    query.push_str(" WHERE id = 1");
    let mut q = sqlx::query(sqlx::AssertSqlSafe(query))
        .bind(id(choices.default_model.as_ref()))
        .bind(choices.concurrency)
        .bind(choices.timeout_seconds);
    for work in Work::ALL {
        q = q.bind(id(choices.get(work)));
    }
    q.execute(db.writer()).await?;
    Ok(())
}

/// Why a request to a model failed.
#[derive(Debug, thiserror::Error)]
pub enum AiError {
    /// No model does this work.
    #[error("no AI model is chosen for this")]
    Off,
    /// The provider failed or refused: its reason, as genai gives it.
    #[error("{0}")]
    Provider(String),
}

impl From<genai::Error> for AiError {
    fn from(e: genai::Error) -> Self {
        AiError::Provider(e.to_string())
    }
}

impl From<reqwest::Error> for AiError {
    fn from(e: reqwest::Error) -> Self {
        // Without the URL: it is in the settings, and an error is logged.
        AiError::Provider(e.without_url().to_string())
    }
}

/// What a test of a model found.
#[derive(Debug, Clone, Default)]
pub struct Checked {
    /// The length of an embeddings model's vectors.
    pub dimensions: Option<usize>,
}

/// The registry made usable: a genai client per model, on one HTTP client,
/// and the permits every request shares.
struct Live {
    registry: Registry,
    http: reqwest::Client,
    clients: Vec<(String, genai::Client)>,
    permits: Arc<Semaphore>,
}

impl Live {
    fn new(registry: Registry) -> Self {
        let http = http_client(registry.choices.timeout_seconds);
        let clients = registry
            .models
            .iter()
            .map(|m| (m.id.clone(), client(m, http.clone())))
            .collect();
        Live {
            permits: Arc::new(Semaphore::new(
                registry.choices.concurrency.clamp(1, MAX_CONCURRENCY) as usize,
            )),
            registry,
            http,
            clients,
        }
    }

    fn client(&self, id: &str) -> Option<&genai::Client> {
        self.clients.iter().find(|(m, _)| m == id).map(|(_, c)| c)
    }
}

fn http_client(timeout_seconds: i64) -> reqwest::Client {
    reqwest::Client::builder()
        // A provider has no reason to redirect an API call, and one that
        // did could send the key elsewhere.
        .redirect(reqwest::redirect::Policy::none())
        .timeout(Duration::from_secs(timeout_seconds.max(1) as u64))
        .build()
        .expect("an HTTP client")
}

/// A genai client bound to a model's provider, on our reqwest, with its key
/// (or none) and its endpoint (or the provider's own).
fn client(model: &Model, http: reqwest::Client) -> genai::Client {
    let adapter = model.adapter().unwrap_or(AdapterKind::OpenAI);
    // Without a key, an empty one: genai's OpenAI adapter refuses to send
    // without any, while a local OpenAI-compatible server (llama.cpp, LM
    // Studio, ...) ignores the empty `Authorization: Bearer` it gives.
    let auth = AuthData::Key(model.api_key.clone());
    // genai joins the service's path (`chat/completions`, ...) to the base
    // URL, which must therefore end with a slash.
    let endpoint = (!model.endpoint.is_empty()).then(|| with_slash(&model.endpoint));
    let resolver_auth = auth.clone();
    genai::Client::builder()
        .with_reqwest(http)
        .with_adapter_kind(adapter)
        // Always our key: never one genai would read from the environment.
        .with_auth_resolver_fn(move |_: ModelIden| Ok(Some(resolver_auth.clone())))
        .with_service_target_resolver_fn(move |target: ServiceTarget| {
            let ServiceTarget {
                endpoint: default,
                model,
                ..
            } = target;
            Ok(ServiceTarget {
                endpoint: endpoint
                    .clone()
                    .map(Endpoint::from_owned)
                    .unwrap_or(default),
                auth: auth.clone(),
                model: ModelIden::new(adapter, model.model_name),
            })
        })
        .build()
        // It fails only when it must build an HTTP client of its own.
        .expect("a genai client on our HTTP client")
}

fn with_slash(url: &str) -> String {
    let mut url = url.to_string();
    if !url.ends_with('/') {
        url.push('/');
    }
    url
}

/// A 1×1 PNG: what a vision model is shown when it is tested.
const TEST_PNG: &str = "iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAADUlEQVR42mP8z8DwHwAFBQIAX8jx0gAAAABJRU5ErkJggg==";

/// A tenth of a second of silence (16 kHz, mono, 16 bits), as WAV: what a
/// speech model is given when it is tested.
fn test_wav() -> Vec<u8> {
    let samples = 1_600u32;
    let data = samples * 2;
    let mut wav = Vec::with_capacity(44 + data as usize);
    wav.extend_from_slice(b"RIFF");
    wav.extend_from_slice(&(36 + data).to_le_bytes());
    wav.extend_from_slice(b"WAVEfmt ");
    wav.extend_from_slice(&16u32.to_le_bytes());
    wav.extend_from_slice(&1u16.to_le_bytes()); // PCM
    wav.extend_from_slice(&1u16.to_le_bytes()); // mono
    wav.extend_from_slice(&16_000u32.to_le_bytes());
    wav.extend_from_slice(&32_000u32.to_le_bytes());
    wav.extend_from_slice(&2u16.to_le_bytes());
    wav.extend_from_slice(&16u16.to_le_bytes());
    wav.extend_from_slice(b"data");
    wav.extend_from_slice(&data.to_le_bytes());
    wav.resize(44 + data as usize, 0);
    wav
}

/// A transcription's answer (`verbose_json`; a plain `json` has the text
/// only).
#[derive(Deserialize)]
struct Transcription {
    #[serde(default)]
    text: String,
    #[serde(default)]
    segments: Option<Vec<Segment>>,
}

#[derive(Deserialize)]
struct Segment {
    #[serde(default)]
    start: f64,
    #[serde(default)]
    text: String,
}

/// How much of a recording is kept: its first hour.
pub const MAX_AUDIO_SECONDS: f64 = 3600.0;

impl Transcription {
    /// The words of the first hour: the segments that start in it when the
    /// server gives them, else the whole text.
    fn first_hour(self) -> String {
        match self.segments {
            Some(segments) if !segments.is_empty() => segments
                .into_iter()
                .filter(|s| s.start < MAX_AUDIO_SECONDS)
                .map(|s| s.text.trim().to_string())
                .filter(|t| !t.is_empty())
                .collect::<Vec<_>>()
                .join(" "),
            _ => self.text.trim().to_string(),
        }
    }
}

/// The registry in effect.
pub struct Ai {
    env: Env,
    live: RwLock<Arc<Live>>,
    /// One save of the settings at a time (`routes/ai.rs`).
    saving: tokio::sync::Mutex<()>,
}

impl Ai {
    /// The registry in effect: the stored models and choices, with the
    /// environment on top. Nothing is asked of the providers until the
    /// first request, so one that is down does not keep the server from
    /// starting.
    pub async fn load(db: &Db, env: Env) -> Self {
        let models = stored_models(db)
            .await
            .unwrap_or_else(|e| panic!("cannot read the AI models: {e}"));
        let choices = stored_choices(db)
            .await
            .unwrap_or_else(|e| panic!("cannot read the AI settings: {e}"));
        Self::new(Registry::new(&env, models, choices), env)
    }

    /// A registry in effect, without a database (the eval harness).
    pub fn new(registry: Registry, env: Env) -> Self {
        crate::install_tls_provider();
        Ai {
            live: RwLock::new(Arc::new(Live::new(registry))),
            env,
            saving: tokio::sync::Mutex::new(()),
        }
    }

    pub fn env(&self) -> &Env {
        &self.env
    }

    fn live(&self) -> Arc<Live> {
        self.live.read().unwrap().clone()
    }

    pub fn registry(&self) -> Registry {
        self.live().registry.clone()
    }

    /// The model of a work; `None` when it is off.
    pub fn model_for(&self, work: Work) -> Option<Model> {
        self.live().registry.model_for(work).cloned()
    }

    /// The model of a work for a user's choice (`Registry::model_chosen`).
    pub fn model_chosen(&self, work: Work, chosen: Option<&str>) -> Option<Model> {
        self.live().registry.model_chosen(work, chosen).cloned()
    }

    /// The id of the model an `AI_MODEL_*` variable locks a work to.
    pub fn locked_model(&self, work: Work) -> Option<String> {
        let live = self.live();
        let (_, name) = live
            .registry
            .locked_names
            .iter()
            .find(|(w, _)| *w == work)?;
        live.registry.by_name(name).map(|m| m.id.clone())
    }

    /// Whether a model (by id) is registered and can do a work.
    pub fn can_do(&self, id: &str, work: Work) -> bool {
        self.live()
            .registry
            .model(id)
            .is_some_and(|m| m.can(work.needs()))
    }

    /// A model does the drop box's proposals: the drop box works.
    pub fn enabled(&self) -> bool {
        self.model_for(Work::Proposals).is_some()
    }

    /// A model does this work.
    pub fn on(&self, work: Work) -> bool {
        self.model_for(work).is_some()
    }

    /// Read the stored models and choices again and put them in effect.
    /// Requests already sent finish on the old clients.
    pub async fn reload(&self, db: &Db) -> Result<(), sqlx::Error> {
        let registry = Registry::new(
            &self.env,
            stored_models(db).await?,
            stored_choices(db).await?,
        );
        *self.live.write().unwrap() = Arc::new(Live::new(registry));
        Ok(())
    }

    pub async fn lock_saves(&self) -> tokio::sync::MutexGuard<'_, ()> {
        self.saving.lock().await
    }

    /// Ask a model's provider whether it works, for each thing it can do: a
    /// tiny prompt (chat), a tiny image (vision), a tenth of a second of
    /// silence (speech), a short text (embeddings).
    pub async fn check(&self, model: &Model) -> Result<Checked, String> {
        model.validate()?;
        let timeout = self.live().registry.choices.timeout_seconds;
        let http = http_client(timeout);
        let genai = client(model, http.clone());
        let mut checked = Checked::default();
        let failed = |what: &str, e: &dyn std::fmt::Display| {
            format!("The model `{}` did not answer {what}: {e}", model.model)
        };
        if model.chat {
            let request = ChatRequest::new(vec![ChatMessage::user("Reply with the word OK.")]);
            genai
                .exec_chat(&model.model, request, None)
                .await
                .map_err(|e| failed("a prompt", &e))?;
        }
        if model.vision {
            let request =
                ChatRequest::new(vec![ChatMessage::user(MessageContent::from_parts(vec![
                    ContentPart::from_text("What colour is this image? One word."),
                    ContentPart::Binary(Binary::from_base64("image/png", TEST_PNG, None)),
                ]))]);
            genai
                .exec_chat(&model.model, request, None)
                .await
                .map_err(|e| failed("about an image", &e))?;
        }
        if model.speech {
            transcribe_with(&genai, &http, model, test_wav(), "test.wav", "audio/wav")
                .await
                .map_err(|e| failed("with a transcription", &e))?;
        }
        if model.embeddings {
            let response = genai
                .embed(&model.model, "Organizer", None)
                .await
                .map_err(|e| failed("with an embedding", &e))?;
            let dimensions = response
                .first_embedding()
                .map(|e| e.vector().len())
                .filter(|n| *n > 0)
                .ok_or_else(|| format!("The model `{}` returned no vector", model.model))?;
            checked.dimensions = Some(dimensions);
        }
        Ok(checked)
    }

    /// One answer of the proposals' model to a system prompt and a message;
    /// as JSON when `json` is set (the prompt must also ask for JSON).
    pub async fn chat(
        &self,
        chosen: Option<&str>,
        system: &str,
        user: &str,
        json: bool,
    ) -> Result<String, AiError> {
        let live = self.live();
        let model = live
            .registry
            .model_chosen(Work::Proposals, chosen)
            .ok_or(AiError::Off)?;
        let client = live.client(&model.id).ok_or(AiError::Off)?;
        let _permit = live
            .permits
            .acquire()
            .await
            .expect("the semaphore is never closed");
        let request = ChatRequest::new(vec![ChatMessage::user(user.to_string())])
            .with_system(system.to_string());
        let options = json.then(|| {
            ChatOptions::default()
                .with_response_format(ChatResponseFormat::JsonMode)
                .with_temperature(0.0)
        });
        let response = client
            .exec_chat(&model.model, request, options.as_ref())
            .await?;
        response
            .into_first_text()
            .ok_or_else(|| AiError::Provider("the model returned no text".into()))
    }

    /// What a vision model (of `Images` or `Scans`) says about an image,
    /// asked with a prompt.
    pub async fn look(
        &self,
        work: Work,
        chosen: Option<&str>,
        prompt: &str,
        image: Binary,
    ) -> Result<String, AiError> {
        let live = self.live();
        let model = live
            .registry
            .model_chosen(work, chosen)
            .ok_or(AiError::Off)?;
        let client = live.client(&model.id).ok_or(AiError::Off)?;
        let _permit = live
            .permits
            .acquire()
            .await
            .expect("the semaphore is never closed");
        let request = ChatRequest::new(vec![ChatMessage::user(MessageContent::from_parts(vec![
            ContentPart::from_text(prompt),
            ContentPart::Binary(image),
        ]))]);
        let options = ChatOptions::default().with_temperature(0.0);
        let response = client
            .exec_chat(&model.model, request, Some(&options))
            .await?;
        Ok(response.into_first_text().unwrap_or_default())
    }

    /// The words of a recording, its first hour, by the audio's model.
    pub async fn transcribe(
        &self,
        chosen: Option<&str>,
        bytes: Vec<u8>,
        name: &str,
        mime: &str,
    ) -> Result<String, AiError> {
        let live = self.live();
        let model = live
            .registry
            .model_chosen(Work::Audio, chosen)
            .ok_or(AiError::Off)?;
        let client = live.client(&model.id).ok_or(AiError::Off)?;
        let _permit = live
            .permits
            .acquire()
            .await
            .expect("the semaphore is never closed");
        transcribe_with(client, &live.http, model, bytes, name, mime).await
    }

    /// The embeddings of some texts, in their order.
    pub async fn embed(&self, texts: Vec<String>) -> Result<Vec<Vec<f32>>, AiError> {
        let live = self.live();
        let model = live
            .registry
            .model_for(Work::Embeddings)
            .ok_or(AiError::Off)?;
        let client = live.client(&model.id).ok_or(AiError::Off)?;
        let _permit = live
            .permits
            .acquire()
            .await
            .expect("the semaphore is never closed");
        let count = texts.len();
        let response = client.embed_batch(&model.model, texts, None).await?;
        let vectors: Vec<Vec<f32>> = response
            .embeddings
            .into_iter()
            .map(|e| e.vector().clone())
            .collect();
        if vectors.len() != count {
            return Err(AiError::Provider(format!(
                "asked for {count} embeddings, got {}",
                vectors.len()
            )));
        }
        Ok(vectors)
    }

    /// The URL a model's requests go to.
    pub async fn endpoint_of(&self, model: &Model) -> String {
        let live = self.live();
        match live.client(&model.id) {
            Some(client) => resolved(client, &model.model).await,
            None => resolved(&client(model, live.http.clone()), &model.model).await,
        }
    }

    /// Where the requests go: what the settings pages show under "What
    /// Organizer sends to the AI".
    pub async fn recipient(&self) -> Recipient {
        let live = self.live();
        let registry = &live.registry;
        let Some(proposals) = registry.model_for(Work::Proposals) else {
            return Recipient::default();
        };
        let mut recipient = Recipient {
            enabled: true,
            provider: proposals.provider.clone(),
            endpoint: self.endpoint_of(proposals).await,
            model: proposals.model.clone(),
            ..Recipient::default()
        };
        if let Some(model) = registry.model_for(Work::Embeddings) {
            recipient.embeddings_model = model.model.clone();
            recipient.embeddings_endpoint = self.endpoint_of(model).await;
        }
        for work in [Work::Images, Work::Scans, Work::Audio] {
            let Some(model) = registry.model_for(work) else {
                continue;
            };
            let destination = Some(Destination {
                name: model.name.clone(),
                provider: model.provider.clone(),
                endpoint: self.endpoint_of(model).await,
                model: model.model.clone(),
            });
            match work {
                Work::Images => recipient.images = destination,
                Work::Scans => recipient.scans = destination,
                _ => recipient.audio = destination,
            }
        }
        recipient
    }
}

async fn resolved(client: &genai::Client, model: &str) -> String {
    client
        .resolve_service_target(model.to_string())
        .await
        .map(|t| t.endpoint.base_url().to_string())
        .unwrap_or_default()
}

/// A transcription by an OpenAI-compatible `/audio/transcriptions`, at the
/// model's endpoint (genai's for the provider when none is set), with its
/// key.
async fn transcribe_with(
    client: &genai::Client,
    http: &reqwest::Client,
    model: &Model,
    bytes: Vec<u8>,
    name: &str,
    mime: &str,
) -> Result<String, AiError> {
    let base = resolved(client, &model.model).await;
    if base.is_empty() {
        return Err(AiError::Provider(
            "no endpoint for the transcription".into(),
        ));
    }
    let url = format!("{}audio/transcriptions", with_slash(&base));
    let file = reqwest::multipart::Part::bytes(bytes)
        .file_name(name.to_string())
        .mime_str(mime)
        .map_err(|e| AiError::Provider(format!("the file's type: {e}")))?;
    let form = reqwest::multipart::Form::new()
        .part("file", file)
        .text("model", model.model.clone())
        .text("response_format", "verbose_json");
    let mut request = http.post(url).multipart(form);
    if !model.api_key.is_empty() {
        request = request.bearer_auth(&model.api_key);
    }
    let response = request.send().await?;
    let status = response.status();
    if !status.is_success() {
        let body = response.text().await.unwrap_or_default();
        return Err(AiError::Provider(format!(
            "the transcription failed ({status}): {}",
            crate::drop_box::extract::truncate(body.trim(), 300)
        )));
    }
    let transcription: Transcription = response.json().await?;
    Ok(transcription.first_hour())
}

/// Where a kind of reading goes.
#[derive(Clone, Debug, Default, Serialize, ToSchema)]
pub struct Destination {
    /// The model's registered name.
    pub name: String,
    pub provider: String,
    /// The URL the requests go to.
    pub endpoint: String,
    /// The provider's name of the model.
    pub model: String,
}

/// Where the requests to the AI go.
#[derive(Clone, Debug, Default, Serialize, ToSchema)]
pub struct Recipient {
    /// A model does the proposals: the drop box works.
    pub enabled: bool,
    /// The proposals' model: its provider, the URL its requests go to, and
    /// the provider's name of it.
    pub provider: String,
    pub endpoint: String,
    pub model: String,
    /// Empty: no embeddings.
    pub embeddings_model: String,
    /// The URL the embeddings requests go to; empty without embeddings.
    pub embeddings_endpoint: String,
    /// Where photos and images go; absent while they are not read.
    pub images: Option<Destination>,
    /// Where the pages of scanned PDFs go; absent while they are not read.
    pub scans: Option<Destination>,
    /// Where recordings go; absent while they are not transcribed.
    pub audio: Option<Destination>,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn chat_model() -> Model {
        Model {
            id: Uuid::now_v7().to_string(),
            name: "Local".into(),
            provider: "openai".into(),
            endpoint: "http://localhost:11434/v1".into(),
            model: "qwen3".into(),
            chat: true,
            ..Model::default()
        }
    }

    #[test]
    fn models_are_checked_before_the_provider_is_asked() {
        let ok = chat_model();
        assert_eq!(ok.validate(), Ok(()));
        let bad = |change: fn(&mut Model)| {
            let mut m = ok.clone();
            change(&mut m);
            m.validate().unwrap_err()
        };
        assert!(bad(|m| m.provider = "skynet".into()).contains("provider must be one of"));
        assert!(bad(|m| m.name = " ".into()).contains("name"));
        assert!(bad(|m| m.model.clear()).contains("provider's name"));
        assert!(bad(|m| m.endpoint = "ftp://x".into()).contains("endpoint must be"));
        assert!(bad(|m| m.chat = false).contains("can do"));
        assert!(
            bad(|m| {
                m.provider = "anthropic".into();
                m.speech = true;
            })
            .contains("OpenAI-compatible")
        );
        let choices = Choices {
            concurrency: 0,
            ..Choices::default()
        };
        assert!(choices.validate().unwrap_err().contains("concurrency"));
    }

    #[test]
    fn the_environment_is_a_locked_default_and_names_lock_the_work() {
        let env = Env {
            provider: Some("openai".into()),
            model: Some("qwen3".into()),
            embeddings_model: Some("nomic".into()),
            images: Some("Eyes".into()),
            timeout_seconds: Some(30),
            ..Env::default()
        };
        assert_eq!(env.validate(), Ok(()));
        let mut eyes = chat_model();
        eyes.name = "Eyes".into();
        eyes.chat = false;
        eyes.vision = true;
        let other = chat_model();
        let choices = Choices {
            default_model: Some(other.id.clone()),
            images: Some(other.id.clone()),
            ..Choices::default()
        };
        let registry = Registry::new(&env, vec![eyes.clone(), other.clone()], choices);
        assert_eq!(registry.default_model().unwrap().id, ENV_MODEL);
        assert_eq!(registry.model_for(Work::Proposals).unwrap().id, ENV_MODEL);
        assert_eq!(
            registry.model_for(Work::Embeddings).unwrap().id,
            ENV_EMBEDDINGS
        );
        assert_eq!(registry.model_for(Work::Images).unwrap().id, eyes.id);
        // Off until chosen; a model that cannot do the work does not.
        assert!(registry.model_for(Work::Audio).is_none());
        assert_eq!(registry.choices.timeout_seconds, 30);
        assert_eq!(env.lock("default"), Some("AI_MODEL"));
        assert_eq!(env.lock("embeddings"), Some("AI_EMBEDDINGS_MODEL"));
        assert_eq!(env.lock("images"), Some("AI_MODEL_IMAGES"));
        assert_eq!(env.lock("scans"), None);

        // Half a model is an error at the start.
        let half = Env {
            model: Some("qwen3".into()),
            ..Env::default()
        };
        assert!(half.validate().unwrap_err().contains("AI_PROVIDER"));
    }

    #[test]
    fn proposals_take_the_default_unless_a_chat_model_is_chosen() {
        let a = chat_model();
        let mut b = chat_model();
        b.name = "B".into();
        let mut embedder = chat_model();
        embedder.name = "E".into();
        embedder.chat = false;
        embedder.embeddings = true;
        let mut choices = Choices {
            default_model: Some(a.id.clone()),
            ..Choices::default()
        };
        let registry = Registry::new(
            &Env::default(),
            vec![a.clone(), b.clone(), embedder.clone()],
            choices.clone(),
        );
        assert_eq!(registry.model_for(Work::Proposals).unwrap().id, a.id);
        assert_eq!(registry.chosen(Work::Proposals), None);
        choices.proposals = Some(b.id.clone());
        choices.embeddings = Some(embedder.id.clone());
        let registry = Registry::new(
            &Env::default(),
            vec![a, b.clone(), embedder.clone()],
            choices,
        );
        assert_eq!(registry.model_for(Work::Proposals).unwrap().id, b.id);
        assert_eq!(registry.chosen(Work::Embeddings), Some(embedder.id));
    }

    #[test]
    fn a_transcription_keeps_its_first_hour() {
        let t: Transcription = serde_json::from_str(
            r#"{"text":"all","segments":[{"start":0,"text":" one "},{"start":3599.5,"text":"two"},{"start":3600,"text":"three"}]}"#,
        )
        .unwrap();
        assert_eq!(t.first_hour(), "one two");
        let t: Transcription = serde_json::from_str(r#"{"text":" plain "}"#).unwrap();
        assert_eq!(t.first_hour(), "plain");
        assert_eq!(test_wav().len(), 44 + 3_200);
    }
}

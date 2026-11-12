//! The superadmin settings of the AI models (`ai.rs`): the registered models
//! (add, change, test, remove), the default model and the model of each
//! kind of work, and reading existing files again; and, for every user,
//! the models they may choose for their own work (`ai.model.*`, names and
//! ids only).

use crate::ai::{self, Capability, Model, Recipient, Work};
use crate::auth::now_iso;
use crate::error::{ErrorBody, HttpError};
use crate::extract::ApiJson;
use crate::routes::auth::{AuthUser, Superadmin};
use crate::state::AppState;
use axum::Json;
use axum::extract::{Path, State};
use axum::http::StatusCode;
use serde::{Deserialize, Serialize};
use utoipa::ToSchema;
use utoipa_axum::router::OpenApiRouter;
use utoipa_axum::routes;
use uuid::Uuid;

/// The models a user may choose, nested under /ai.
pub fn router() -> OpenApiRouter<AppState> {
    OpenApiRouter::new().routes(routes!(list_selectable_ai_models))
}

/// The AI settings, nested under /superadmin.
pub fn superadmin_router() -> OpenApiRouter<AppState> {
    OpenApiRouter::new()
        .routes(routes!(get_settings, update_settings))
        .routes(routes!(add_model))
        .routes(routes!(update_model, remove_model))
        .routes(routes!(test_model))
        .routes(routes!(read_again))
}

/// A model, registered or from the environment. Its API key is never sent
/// back.
#[derive(Serialize, ToSchema)]
pub struct AiModel {
    /// A registered model's id, or `env` / `env-embeddings` for the models
    /// of the AI_* variables.
    pub id: String,
    pub name: String,
    /// The genai adapter: `openai` (any OpenAI-compatible API), `ollama`,
    /// `anthropic`, `gemini`, ...
    pub provider: String,
    /// The base URL; empty for the provider's own.
    pub endpoint: String,
    /// Whether an API key is set.
    pub api_key_set: bool,
    /// The provider's name of the model.
    pub model: String,
    pub capabilities: Vec<Capability>,
    /// Set by the AI_* variables (listed in `variables`): it cannot be
    /// changed or removed here.
    pub from_environment: bool,
    /// The variables that make it; empty for a registered model.
    pub variables: Vec<String>,
    /// The URL its requests go to.
    pub sends_to: String,
}

/// The model of each kind of work: a model's id; null for the default
/// (proposals) or off (the rest).
#[derive(Serialize, ToSchema)]
pub struct AiWork {
    pub proposals: Option<String>,
    pub embeddings: Option<String>,
    pub images: Option<String>,
    pub scans: Option<String>,
    pub audio: Option<String>,
}

/// A choice an environment variable makes.
#[derive(Serialize, ToSchema)]
pub struct AiLock {
    /// `default`, a work (`embeddings`, `images`, `scans`, `audio`),
    /// `concurrency` or `timeout_seconds`.
    pub key: String,
    /// The variable, e.g. `AI_MODEL_IMAGES`.
    pub variable: String,
    /// For a variable that names a model: the name it gives.
    pub value: Option<String>,
}

/// The AI settings in effect.
#[derive(Serialize, ToSchema)]
pub struct AiSettings {
    /// A model does the drop box's proposals: the drop box works.
    pub enabled: bool,
    /// The models, the environment's first, then the registered ones by
    /// name.
    pub models: Vec<AiModel>,
    /// The default model's id: a chat model that does the proposals unless
    /// another is chosen; null when there is none.
    pub default_model: Option<String>,
    pub work: AiWork,
    /// How many requests at once, to all the models together.
    pub concurrency: i64,
    /// The longest a request may take, in seconds.
    pub timeout_seconds: i64,
    /// The choices set by an environment variable, which cannot be changed
    /// here.
    pub locked: Vec<AiLock>,
    /// The providers to choose from.
    pub providers: Vec<String>,
    /// Where the requests go, for the description of what is sent.
    pub recipient: Recipient,
}

/// The choices to change; a field left out stays as it is. A model's id;
/// an empty string for the default (proposals), off (the rest), or no
/// default model.
#[derive(Deserialize, ToSchema)]
pub struct UpdateAiSettings {
    pub default_model: Option<String>,
    pub proposals: Option<String>,
    pub embeddings: Option<String>,
    pub images: Option<String>,
    pub scans: Option<String>,
    pub audio: Option<String>,
    pub concurrency: Option<i64>,
    pub timeout_seconds: Option<i64>,
}

/// A model to add, or the fields of one to change (a field left out stays
/// as it is).
#[derive(Deserialize, ToSchema, Default)]
pub struct AiModelInput {
    pub name: Option<String>,
    pub provider: Option<String>,
    /// Empty for the provider's own.
    pub endpoint: Option<String>,
    /// An empty string removes the key. A new provider or endpoint removes
    /// it too, unless one is sent with it: the key is for one service only.
    pub api_key: Option<String>,
    /// The provider's name of the model.
    pub model: Option<String>,
    pub capabilities: Option<Vec<Capability>>,
}

/// A model to test: a registered one (`id`, with any changes on top), or a
/// new one (without `id`).
#[derive(Deserialize, ToSchema)]
pub struct AiModelTest {
    pub id: Option<String>,
    #[serde(flatten)]
    pub model: AiModelInput,
}

/// What a test found.
#[derive(Serialize, ToSchema)]
pub struct AiTestResult {
    /// The length of an embeddings model's vectors; absent otherwise.
    pub embedding_dimensions: Option<usize>,
}

/// How many files are to be read again.
#[derive(Serialize, ToSchema)]
pub struct ReadAgain {
    pub queued: i64,
}

/// A model one may choose: its name and id, nothing of where it is or how
/// it is reached.
#[derive(Serialize, ToSchema)]
pub struct AiModelChoice {
    pub id: String,
    pub name: String,
}

/// The models one may choose for each kind of work with a user level
/// (`ai.model.proposals`, `.images`, `.scans`, `.audio`): those that can
/// do it, by name.
#[derive(Serialize, ToSchema)]
pub struct AiModelChoices {
    pub proposals: Vec<AiModelChoice>,
    pub images: Vec<AiModelChoice>,
    pub scans: Vec<AiModelChoice>,
    pub audio: Vec<AiModelChoice>,
}

#[utoipa::path(
    get,
    path = "/models",
    tag = "Settings",
    operation_id = "listSelectableAiModels",
    summary = "The AI models I may choose",
    description = "For each kind of work a user, a team or a drive may choose a model for (the `ai.model.*` settings): the superadmin's models that can do it, by name, with their ids. Never where a model is or its key.",
    responses(
        (status = 200, description = "The models by kind of work", body = AiModelChoices),
        (status = 401, description = "Not signed in", body = ErrorBody),
    ),
    security(("session" = []), ("api_key" = [])),
)]
async fn list_selectable_ai_models(
    State(state): State<AppState>,
    _: AuthUser,
) -> Json<AiModelChoices> {
    let registry = state.ai.registry();
    let able = |work: Work| {
        let mut models: Vec<AiModelChoice> = registry
            .models
            .iter()
            .filter(|m| m.can(work.needs()))
            .map(|m| AiModelChoice {
                id: m.id.clone(),
                name: m.name.clone(),
            })
            .collect();
        models.sort_by_key(|m| m.name.to_lowercase());
        models
    };
    Json(AiModelChoices {
        proposals: able(Work::Proposals),
        images: able(Work::Images),
        scans: able(Work::Scans),
        audio: able(Work::Audio),
    })
}

async fn model_view(state: &AppState, model: &Model) -> AiModel {
    let from_environment = model.from_env();
    let env = state.ai.env();
    let variables = if from_environment {
        let mut vars = env.model_variables();
        // The embeddings model is not made by AI_MODEL, nor the chat model
        // by AI_EMBEDDINGS_*.
        vars.retain(|v| match model.id.as_str() {
            ai::ENV_MODEL => !v.starts_with("AI_EMBEDDINGS"),
            _ => *v != "AI_MODEL",
        });
        vars.into_iter().map(String::from).collect()
    } else {
        Vec::new()
    };
    AiModel {
        id: model.id.clone(),
        name: model.name.clone(),
        provider: model.provider.clone(),
        endpoint: model.endpoint.clone(),
        api_key_set: !model.api_key.is_empty(),
        model: model.model.clone(),
        capabilities: model.capabilities(),
        from_environment,
        variables,
        sends_to: state.ai.endpoint_of(model).await,
    }
}

async fn view(state: &AppState) -> AiSettings {
    let registry = state.ai.registry();
    let mut models = Vec::new();
    for model in &registry.models {
        models.push(model_view(state, model).await);
    }
    let env = state.ai.env();
    let locked = env
        .locked()
        .into_iter()
        .map(|(key, variable)| AiLock {
            key: key.into(),
            variable: variable.into(),
            value: match variable {
                "AI_MODEL_IMAGES" => env.images.clone(),
                "AI_MODEL_SCANS" => env.scans.clone(),
                "AI_MODEL_AUDIO" => env.audio.clone(),
                "AI_MODEL_EMBEDDINGS" => env.embeddings.clone(),
                _ => None,
            },
        })
        .collect();
    AiSettings {
        enabled: registry.model_for(Work::Proposals).is_some(),
        models,
        default_model: registry.default_model().map(|m| m.id.clone()),
        work: AiWork {
            proposals: registry.chosen(Work::Proposals),
            embeddings: registry.chosen(Work::Embeddings),
            images: registry.chosen(Work::Images),
            scans: registry.chosen(Work::Scans),
            audio: registry.chosen(Work::Audio),
        },
        concurrency: registry.choices.concurrency,
        timeout_seconds: registry.choices.timeout_seconds,
        locked,
        providers: ai::providers().into_iter().map(String::from).collect(),
        recipient: state.ai.recipient().await,
    }
}

#[utoipa::path(
    get,
    path = "/ai",
    tag = "Superadmin",
    operation_id = "getAiSettings",
    summary = "The AI models and their work",
    responses(
        (status = 200, description = "The settings in effect", body = AiSettings),
        (status = 401, description = "Not signed in", body = ErrorBody),
        (status = 403, description = "Superadmin only, with a session (never a token) whose second factor was confirmed within 15 minutes: else `code` is `step_up_required` (confirm it, `/auth/step-up/…`) or `second_factor_required` (add one first)", body = ErrorBody),
    ),
    security(("session" = [])),
)]
async fn get_settings(
    State(state): State<AppState>,
    _: Superadmin,
) -> Result<Json<AiSettings>, HttpError> {
    Ok(Json(view(&state).await))
}

#[utoipa::path(
    put,
    path = "/ai",
    tag = "Superadmin",
    operation_id = "updateAiSettings",
    summary = "Choose the default model and the model of each work",
    description = "Takes effect at once. Each model must be able to do its work (a chat model for the default and the proposals, embeddings, vision for images and scans, speech for audio). A choice set by an environment variable cannot be changed (400).",
    request_body(content = UpdateAiSettings, content_type = "application/json"),
    responses(
        (status = 200, description = "The settings now in effect", body = AiSettings),
        (status = 400, description = "A model that cannot do the work, or a locked choice", body = ErrorBody),
        (status = 401, description = "Not signed in", body = ErrorBody),
        (status = 403, description = "Superadmin only, with a session (never a token) whose second factor was confirmed within 15 minutes: else `code` is `step_up_required` (confirm it, `/auth/step-up/…`) or `second_factor_required` (add one first)", body = ErrorBody),
        (status = 415, description = "The body must be JSON (Content-Type: application/json)", body = ErrorBody),
    ),
    security(("session" = [])),
)]
async fn update_settings(
    State(state): State<AppState>,
    _: Superadmin,
    ApiJson(body): ApiJson<UpdateAiSettings>,
) -> Result<Json<AiSettings>, HttpError> {
    // A task of its own, which runs to the end even if this request is
    // dropped half way: what is stored is always what is in effect.
    tokio::spawn(save_choices(state.clone(), body))
        .await
        .map_err(|e| HttpError::internal(format!("saving failed: {e}")))??;
    Ok(Json(view(&state).await))
}

async fn save_choices(state: AppState, body: UpdateAiSettings) -> Result<(), HttpError> {
    let _saving = state.ai.lock_saves().await;
    let before = state.ai.registry();
    let mut choices = ai::stored_choices(&state.db).await?;
    let env = state.ai.env();
    let check_lock = |key: &str| match env.lock(key) {
        Some(variable) => Err(HttpError::bad_request(format!(
            "{key} is set by the {variable} environment variable"
        ))),
        None => Ok(()),
    };
    // A model's id (or empty), checked against what it must do.
    let pick = |id: String, needs: Capability| -> Result<Option<String>, HttpError> {
        let id = id.trim().to_string();
        if id.is_empty() {
            return Ok(None);
        }
        match before.model(&id) {
            Some(model) if model.can(needs) => Ok(Some(id)),
            Some(model) => Err(HttpError::bad_request(format!(
                "`{}` cannot do this: it needs {}",
                model.name,
                match needs {
                    Capability::Chat => "chat",
                    Capability::Vision => "vision",
                    Capability::Speech => "speech",
                    Capability::Embeddings => "embeddings",
                }
            ))),
            None => Err(HttpError::bad_request("No such model")),
        }
    };
    if let Some(id) = body.default_model {
        check_lock("default")?;
        choices.default_model = pick(id, Capability::Chat)?;
    }
    for (work, value) in [
        (Work::Proposals, body.proposals),
        (Work::Embeddings, body.embeddings),
        (Work::Images, body.images),
        (Work::Scans, body.scans),
        (Work::Audio, body.audio),
    ] {
        let Some(id) = value else { continue };
        check_lock(work.as_str())?;
        choices.set(work, pick(id, work.needs())?);
    }
    if let Some(n) = body.concurrency {
        check_lock("concurrency")?;
        choices.concurrency = n;
    }
    if let Some(n) = body.timeout_seconds {
        check_lock("timeout_seconds")?;
        choices.timeout_seconds = n;
    }
    choices.validate().map_err(HttpError::bad_request)?;
    ai::store_choices(&state.db, &choices).await?;
    put_in_effect(&state, &before).await
}

/// Read the registry again and put it in effect; then what follows from
/// the change: a new embeddings model embeds every tree again, and items
/// that waited for a model are analysed.
async fn put_in_effect(state: &AppState, before: &ai::Registry) -> Result<(), HttpError> {
    state.ai.reload(&state.db).await?;
    let after = state.ai.registry();
    let embedder = |r: &ai::Registry| {
        r.model_for(Work::Embeddings)
            .map(|m| (m.provider.clone(), m.endpoint.clone(), m.model.clone()))
    };
    if embedder(&after).is_some() && embedder(&after) != embedder(before) {
        let users: Vec<Uuid> = sqlx::query_scalar("SELECT id FROM users")
            .fetch_all(state.db.reader())
            .await?;
        for user in users {
            state.files.announce(user, crate::files::Topic::Reindex);
        }
    }
    if after.model_for(Work::Proposals).is_some()
        && let Err(e) = crate::drop_box::resume_waiting(state).await
    {
        tracing::warn!("cannot queue the drop box items that waited: {e}");
    }
    Ok(())
}

/// A model with the changes of an input on top. A new provider or endpoint
/// drops the key unless one comes with it.
fn apply(mut model: Model, input: AiModelInput) -> Model {
    let before = (model.provider.clone(), model.endpoint.clone());
    if let Some(name) = input.name {
        model.name = name.trim().to_string();
    }
    if let Some(provider) = input.provider {
        model.provider = provider.trim().to_lowercase();
    }
    if let Some(endpoint) = input.endpoint {
        model.endpoint = endpoint.trim().to_string();
    }
    if let Some(name) = input.model {
        model.model = name.trim().to_string();
    }
    if let Some(capabilities) = input.capabilities {
        model.set_capabilities(&capabilities);
    }
    match input.api_key {
        Some(key) => model.api_key = key.trim().to_string(),
        None if (model.provider.clone(), model.endpoint.clone()) != before => model.api_key.clear(),
        None => {}
    }
    model
}

/// A registered model of the superadmin's, by id: 404 for anything else (the
/// environment's are not the superadmin's to change).
fn registered(state: &AppState, id: &str) -> Result<Model, HttpError> {
    let model = state
        .ai
        .registry()
        .model(id)
        .cloned()
        .ok_or_else(HttpError::not_found)?;
    if model.from_env() {
        return Err(HttpError::bad_request(
            "This model is set by the AI_* environment variables",
        ));
    }
    Ok(model)
}

/// No other model of the superadmin's has this name.
fn name_is_free(state: &AppState, name: &str, except: Option<&str>) -> Result<(), HttpError> {
    let taken = state
        .ai
        .registry()
        .models
        .iter()
        .any(|m| m.name.eq_ignore_ascii_case(name) && Some(m.id.as_str()) != except);
    if taken {
        return Err(HttpError::conflict(format!(
            "A model is already called `{name}`"
        )));
    }
    Ok(())
}

#[utoipa::path(
    post,
    path = "/ai/models",
    tag = "Superadmin",
    operation_id = "addAiModel",
    summary = "Register a model",
    description = "The model must answer first, for each thing it can do (a tiny prompt, a tiny image, a tenth of a second of silence, a short text to embed), or nothing is stored (400, with the provider's reason). The first chat model becomes the default.",
    request_body(content = AiModelInput, content_type = "application/json"),
    responses(
        (status = 201, description = "Registered", body = AiModel),
        (status = 400, description = "The model is invalid, or does not answer", body = ErrorBody),
        (status = 401, description = "Not signed in", body = ErrorBody),
        (status = 403, description = "Superadmin only, with a session (never a token) whose second factor was confirmed within 15 minutes: else `code` is `step_up_required` (confirm it, `/auth/step-up/…`) or `second_factor_required` (add one first)", body = ErrorBody),
        (status = 409, description = "A model of that name exists", body = ErrorBody),
        (status = 415, description = "The body must be JSON (Content-Type: application/json)", body = ErrorBody),
    ),
    security(("session" = [])),
)]
async fn add_model(
    State(state): State<AppState>,
    _: Superadmin,
    ApiJson(body): ApiJson<AiModelInput>,
) -> Result<(StatusCode, Json<AiModel>), HttpError> {
    let id = tokio::spawn(save_model(state.clone(), None, body))
        .await
        .map_err(|e| HttpError::internal(format!("saving failed: {e}")))??;
    let model = state
        .ai
        .registry()
        .model(&id)
        .cloned()
        .ok_or_else(|| HttpError::internal("the model is gone"))?;
    Ok((StatusCode::CREATED, Json(model_view(&state, &model).await)))
}

#[utoipa::path(
    put,
    path = "/ai/models/{id}",
    tag = "Superadmin",
    operation_id = "updateAiModel",
    summary = "Change a registered model",
    description = "Tested first, as when it is added. A new provider or endpoint removes the API key unless one is sent with it.",
    params(("id" = String, Path, description = "The model's id")),
    request_body(content = AiModelInput, content_type = "application/json"),
    responses(
        (status = 200, description = "Changed", body = AiModel),
        (status = 400, description = "The model is invalid, does not answer, or is the environment's", body = ErrorBody),
        (status = 401, description = "Not signed in", body = ErrorBody),
        (status = 403, description = "Superadmin only, with a session (never a token) whose second factor was confirmed within 15 minutes: else `code` is `step_up_required` (confirm it, `/auth/step-up/…`) or `second_factor_required` (add one first)", body = ErrorBody),
        (status = 404, description = "No such model", body = ErrorBody),
        (status = 409, description = "Another model has that name", body = ErrorBody),
        (status = 415, description = "The body must be JSON (Content-Type: application/json)", body = ErrorBody),
    ),
    security(("session" = [])),
)]
async fn update_model(
    State(state): State<AppState>,
    _: Superadmin,
    Path(id): Path<String>,
    ApiJson(body): ApiJson<AiModelInput>,
) -> Result<Json<AiModel>, HttpError> {
    let id = tokio::spawn(save_model(state.clone(), Some(id), body))
        .await
        .map_err(|e| HttpError::internal(format!("saving failed: {e}")))??;
    let model = state
        .ai
        .registry()
        .model(&id)
        .cloned()
        .ok_or_else(|| HttpError::internal("the model is gone"))?;
    Ok(Json(model_view(&state, &model).await))
}

/// Add or change a model, tested first, one save at a time. Answers its id.
async fn save_model(
    state: AppState,
    id: Option<String>,
    input: AiModelInput,
) -> Result<String, HttpError> {
    let _saving = state.ai.lock_saves().await;
    let before = state.ai.registry();
    let (model, new) = match &id {
        Some(id) => (apply(registered(&state, id)?, input), false),
        None => {
            if input.name.is_none() || input.provider.is_none() || input.model.is_none() {
                return Err(HttpError::bad_request(
                    "A model needs a name, a provider and the provider's name of the model",
                ));
            }
            let blank = Model {
                id: Uuid::now_v7().to_string(),
                ..Model::default()
            };
            (apply(blank, input), true)
        }
    };
    model.validate().map_err(HttpError::bad_request)?;
    name_is_free(&state, &model.name, Some(&model.id))?;
    // The provider is asked before anything is stored, and no connection
    // is held while it answers.
    state
        .ai
        .check(&model)
        .await
        .map_err(HttpError::bad_request)?;
    let uuid = Uuid::parse_str(&model.id).map_err(|_| HttpError::not_found())?;
    if new {
        sqlx::query(
            "INSERT INTO ai_models (id, name, provider, endpoint, api_key, model, chat, vision,
               speech, embeddings, created_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)",
        )
        .bind(uuid)
        .bind(&model.name)
        .bind(&model.provider)
        .bind(&model.endpoint)
        .bind(&model.api_key)
        .bind(&model.model)
        .bind(model.chat)
        .bind(model.vision)
        .bind(model.speech)
        .bind(model.embeddings)
        .bind(now_iso())
        .execute(state.db.writer())
        .await
        .map_err(|e| match e {
            sqlx::Error::Database(d) if d.is_unique_violation() => {
                HttpError::conflict(format!("A model is already called `{}`", model.name))
            }
            e => e.into(),
        })?;
        // The first chat model is the default.
        if model.chat {
            let mut choices = ai::stored_choices(&state.db).await?;
            let current = choices
                .default_model
                .as_deref()
                .and_then(|id| Uuid::parse_str(id).ok());
            let current_is_chat = match current {
                Some(id) => {
                    sqlx::query_scalar::<_, bool>(
                        "SELECT EXISTS (SELECT 1 FROM ai_models WHERE id = ?1 AND chat = 1)",
                    )
                    .bind(id)
                    .fetch_one(state.db.reader())
                    .await?
                }
                None => false,
            };
            if !current_is_chat {
                choices.default_model = Some(uuid.to_string());
                ai::store_choices(&state.db, &choices).await?;
            }
        }
    } else {
        sqlx::query(
            "UPDATE ai_models SET name = ?2, provider = ?3, endpoint = ?4, api_key = ?5,
               model = ?6, chat = ?7, vision = ?8, speech = ?9, embeddings = ?10
             WHERE id = ?1",
        )
        .bind(uuid)
        .bind(&model.name)
        .bind(&model.provider)
        .bind(&model.endpoint)
        .bind(&model.api_key)
        .bind(&model.model)
        .bind(model.chat)
        .bind(model.vision)
        .bind(model.speech)
        .bind(model.embeddings)
        .execute(state.db.writer())
        .await
        .map_err(|e| match e {
            sqlx::Error::Database(d) if d.is_unique_violation() => {
                HttpError::conflict(format!("A model is already called `{}`", model.name))
            }
            e => e.into(),
        })?;
    }
    put_in_effect(&state, &before).await?;
    Ok(model.id)
}

#[utoipa::path(
    delete,
    path = "/ai/models/{id}",
    tag = "Superadmin",
    operation_id = "removeAiModel",
    summary = "Remove a registered model",
    description = "The work it did goes back to the default (proposals) or off. Refused (400) for the environment's models and for a model an AI_MODEL_* variable names.",
    params(("id" = String, Path, description = "The model's id")),
    responses(
        (status = 204, description = "Removed"),
        (status = 400, description = "The model is set or named by an environment variable", body = ErrorBody),
        (status = 401, description = "Not signed in", body = ErrorBody),
        (status = 403, description = "Superadmin only, with a session (never a token) whose second factor was confirmed within 15 minutes: else `code` is `step_up_required` (confirm it, `/auth/step-up/…`) or `second_factor_required` (add one first)", body = ErrorBody),
        (status = 404, description = "No such model", body = ErrorBody),
    ),
    security(("session" = [])),
)]
async fn remove_model(
    State(state): State<AppState>,
    _: Superadmin,
    Path(id): Path<String>,
) -> Result<StatusCode, HttpError> {
    tokio::spawn(async move {
        let _saving = state.ai.lock_saves().await;
        let before = state.ai.registry();
        let model = registered(&state, &id)?;
        let env = state.ai.env();
        for (variable, name) in [
            ("AI_MODEL_IMAGES", &env.images),
            ("AI_MODEL_SCANS", &env.scans),
            ("AI_MODEL_AUDIO", &env.audio),
            ("AI_MODEL_EMBEDDINGS", &env.embeddings),
        ] {
            if name.as_deref() == Some(model.name.as_str()) {
                return Err(HttpError::bad_request(format!(
                    "{variable} names this model"
                )));
            }
        }
        let uuid = Uuid::parse_str(&model.id).map_err(|_| HttpError::not_found())?;
        // The choices that named it are cleared by their foreign keys.
        sqlx::query("DELETE FROM ai_models WHERE id = ?1")
            .bind(uuid)
            .execute(state.db.writer())
            .await?;
        put_in_effect(&state, &before).await
    })
    .await
    .map_err(|e| HttpError::internal(format!("removing failed: {e}")))??;
    Ok(StatusCode::NO_CONTENT)
}

#[utoipa::path(
    post,
    path = "/ai/models/test",
    tag = "Superadmin",
    operation_id = "testAiModel",
    summary = "Test a model",
    description = "A registered model (with `id`; the other fields, when given, are changes on top, the key kept as a save would keep it), a model of the environment (`env`, `env-embeddings`), or a new one: tested as a save would test it, but not stored.",
    request_body(content = AiModelTest, content_type = "application/json"),
    responses(
        (status = 200, description = "The model answered", body = AiTestResult),
        (status = 400, description = "The model is invalid, or does not answer", body = ErrorBody),
        (status = 401, description = "Not signed in", body = ErrorBody),
        (status = 403, description = "Superadmin only, with a session (never a token) whose second factor was confirmed within 15 minutes: else `code` is `step_up_required` (confirm it, `/auth/step-up/…`) or `second_factor_required` (add one first)", body = ErrorBody),
        (status = 404, description = "No such model", body = ErrorBody),
        (status = 415, description = "The body must be JSON (Content-Type: application/json)", body = ErrorBody),
    ),
    security(("session" = [])),
)]
async fn test_model(
    State(state): State<AppState>,
    _: Superadmin,
    ApiJson(body): ApiJson<AiModelTest>,
) -> Result<Json<AiTestResult>, HttpError> {
    let model = match &body.id {
        Some(id) => {
            let model = state
                .ai
                .registry()
                .model(id)
                .cloned()
                .ok_or_else(HttpError::not_found)?;
            if model.from_env() {
                model
            } else {
                apply(model, body.model)
            }
        }
        None => apply(
            Model {
                name: "test".into(),
                ..Model::default()
            },
            body.model,
        ),
    };
    let checked = state
        .ai
        .check(&model)
        .await
        .map_err(HttpError::bad_request)?;
    Ok(Json(AiTestResult {
        embedding_dimensions: checked.dimensions,
    }))
}

#[utoipa::path(
    post,
    path = "/ai/read-again",
    tag = "Superadmin",
    operation_id = "readFilesAgain",
    summary = "Read existing files again",
    description = "Every user's photos, scanned PDFs and recordings that their models have not read yet are read, by the models chosen now, as the users' settings and sensitive-file rules allow. What was read once is not read again.",
    responses(
        (status = 200, description = "Queued", body = ReadAgain),
        (status = 401, description = "Not signed in", body = ErrorBody),
        (status = 403, description = "Superadmin only, with a session (never a token) whose second factor was confirmed within 15 minutes: else `code` is `step_up_required` (confirm it, `/auth/step-up/…`) or `second_factor_required` (add one first)", body = ErrorBody),
    ),
    security(("session" = [])),
)]
async fn read_again(
    State(state): State<AppState>,
    _: Superadmin,
) -> Result<Json<ReadAgain>, HttpError> {
    let queued = crate::reading::read_again(&state).await?;
    Ok(Json(ReadAgain { queued }))
}

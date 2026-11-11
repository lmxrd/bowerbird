//! `GET /search`: names, paths and the text of files (`search.rs`).

use crate::error::{ErrorBody, HttpError};
use crate::routes::auth::AuthUser;
use crate::search::{self, Hit};
use crate::state::AppState;
use axum::Json;
use axum::extract::{Query, State};
use serde::Deserialize;
use utoipa::IntoParams;
use utoipa_axum::router::OpenApiRouter;
use utoipa_axum::routes;

pub fn router() -> OpenApiRouter<AppState> {
    OpenApiRouter::new().routes(routes!(search_files))
}

/// A search.
#[derive(Deserialize, IntoParams)]
#[into_params(parameter_in = Query)]
pub struct SearchParams {
    /// The words to find: each one, or a word it begins (`elec` finds
    /// "electricity"), in a name, a folder's path, or a file's text.
    pub q: String,
    /// At most this many (default 50, at most 200).
    pub limit: Option<i64>,
    /// Also files whose text is near in meaning (with an embeddings model
    /// set up), after those found by their words.
    #[serde(default)]
    pub semantic: bool,
}

#[utoipa::path(
    get,
    path = "/search",
    tag = "Search",
    operation_id = "search",
    summary = "Search the files",
    description = "The best matches first: a word in the name counts most, then in the folder's path, then in the text read from the file. The text is read in the background, when the drop box settings allow reading contents, so a new file is found by its text a little later than by its name. With `semantic` and an embeddings model, files whose text is near in meaning follow (`semantic: true`).",
    params(SearchParams),
    responses(
        (status = 200, description = "The matches", body = Vec<Hit>),
        (status = 400, description = "An invalid parameter", body = ErrorBody),
        (status = 401, description = "Not signed in", body = ErrorBody),
    ),
    security(("session" = []), ("api_key" = [])),
)]
async fn search_files(
    State(state): State<AppState>,
    AuthUser(user, _): AuthUser,
    Query(params): Query<SearchParams>,
) -> Result<Json<Vec<Hit>>, HttpError> {
    let limit = params.limit.unwrap_or(50).clamp(1, 200);
    let mut hits = search::search(&state, user.id, &params.q, limit).await?;
    if params.semantic && (hits.len() as i64) < limit {
        let found = hits.iter().map(|h| h.node_id).collect();
        let more = limit as usize - hits.len();
        match search::semantic(&state, user.id, &params.q, more, &found).await {
            Ok(extra) => hits.extend(extra),
            // The words' matches stand alone when the endpoint fails.
            Err(e) => tracing::warn!("semantic search failed: {e}"),
        }
    }
    Ok(Json(hits))
}

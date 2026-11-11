use axum::Json;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use serde::Serialize;
use utoipa::ToSchema;

/// The `code` of [`HttpError::step_up_required`].
pub const STEP_UP_REQUIRED: &str = "step_up_required";
/// The `code` of [`HttpError::second_factor_required`].
pub const SECOND_FACTOR_REQUIRED: &str = "second_factor_required";
/// The `code` of a 503 for a drive whose storage fails or is locked.
pub const STORAGE_UNAVAILABLE: &str = "storage_unavailable";
/// The `code` of a 409 for a write to a drive being moved to other storage.
pub const READ_ONLY: &str = "read_only";

/// The body of every error response: `{"error": "..."}`, and a `code` for
/// the few refusals a client acts on rather than shows.
#[derive(Serialize, ToSchema)]
pub struct ErrorBody {
    /// What went wrong, fit to show to a person.
    pub error: String,
    /// What a client may act on, left out for every other error:
    /// `step_up_required` (confirm the passkey or the two-factor code
    /// again, `/auth/step-up/…`, then retry) or `second_factor_required`
    /// (add a passkey or two-factor authentication first), with a 503
    /// `storage_unavailable` (the drive's storage fails or is locked: try
    /// again later), with a 409 `read_only` (the drive is being moved to
    /// other storage: try again once that is done).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub code: Option<String>,
}

impl ErrorBody {
    pub fn new(error: impl Into<String>) -> Self {
        Self {
            error: error.into(),
            code: None,
        }
    }
}

/// An error answered as an [`ErrorBody`] with its status, the shape every
/// frontend call understands. What the client may know is a `Status` with
/// its message; a failure on our side (the database, or anything else) is
/// logged with its causes and answered with a plain 500, so no internal
/// detail reaches the client.
#[derive(Debug, thiserror::Error)]
pub enum HttpError {
    /// An answer for the client: the status, a message fit to show, and the
    /// code a client acts on, if any (see [`ErrorBody`]).
    #[error("{status}: {message}")]
    Status {
        status: StatusCode,
        message: String,
        code: Option<&'static str>,
    },
    /// Too many attempts: try again in this many seconds.
    #[error("too many attempts, retry after {retry_after} s")]
    TooManyRequests { retry_after: u64 },
    /// The database failed.
    #[error("database error")]
    Database(#[from] sqlx::Error),
    /// Something else failed on our side.
    #[error(transparent)]
    Internal(#[from] anyhow::Error),
}

impl HttpError {
    pub fn new(status: StatusCode, message: impl Into<String>) -> Self {
        Self::Status {
            status,
            message: message.into(),
            code: None,
        }
    }

    /// A superadmin endpoint, and the second factor was not confirmed
    /// within the last 15 minutes: the client asks for it, then retries.
    pub fn step_up_required() -> Self {
        Self::Status {
            status: StatusCode::FORBIDDEN,
            message: "Confirm it's you: your passkey or a two-factor code".into(),
            code: Some(STEP_UP_REQUIRED),
        }
    }

    /// A superadmin endpoint, for a superadmin who has no second factor to
    /// confirm: they add a passkey or two-factor authentication first.
    pub fn second_factor_required() -> Self {
        Self::Status {
            status: StatusCode::FORBIDDEN,
            message: "Add a passkey or two-factor authentication first".into(),
            code: Some(SECOND_FACTOR_REQUIRED),
        }
    }

    pub fn bad_request(message: impl Into<String>) -> Self {
        Self::new(StatusCode::BAD_REQUEST, message)
    }

    pub fn unauthorized() -> Self {
        Self::from_status(StatusCode::UNAUTHORIZED)
    }

    pub fn forbidden() -> Self {
        Self::from_status(StatusCode::FORBIDDEN)
    }

    /// Password sign-in is turned off (`OIDC_ONLY`).
    pub fn passwords_off() -> Self {
        Self::new(
            StatusCode::FORBIDDEN,
            "Password sign-in is turned off: use single sign-on",
        )
    }

    /// A superadmin disabled the account: told only once the password was
    /// right.
    pub fn account_disabled() -> Self {
        Self::new(
            StatusCode::FORBIDDEN,
            "This account is disabled: ask your superadmin",
        )
    }

    pub fn not_found() -> Self {
        Self::new(StatusCode::NOT_FOUND, "Not found")
    }

    pub fn conflict(message: impl Into<String>) -> Self {
        Self::new(StatusCode::CONFLICT, message)
    }

    /// An error that needs no more words than its status: the message is
    /// the status's canonical reason.
    fn from_status(status: StatusCode) -> Self {
        Self::new(status, status.canonical_reason().unwrap_or("Error"))
    }

    /// A failure on our side, with what failed for the log; the client
    /// gets a plain 500.
    pub fn internal(message: impl std::fmt::Display + Send + Sync + 'static) -> Self {
        Self::Internal(anyhow::anyhow!("{message}"))
    }

    /// The status this error is answered with.
    pub fn status(&self) -> StatusCode {
        match self {
            Self::Status { status, .. } => *status,
            Self::TooManyRequests { .. } => StatusCode::TOO_MANY_REQUESTS,
            Self::Database(_) | Self::Internal(_) => StatusCode::INTERNAL_SERVER_ERROR,
        }
    }
}

impl IntoResponse for HttpError {
    fn into_response(self) -> Response {
        let status = self.status();
        let error = match self {
            Self::Status { message, code, .. } => {
                let body = ErrorBody {
                    error: message,
                    code: code.map(str::to_string),
                };
                return (status, Json(body)).into_response();
            }
            Self::TooManyRequests { retry_after } => {
                let body = Json(ErrorBody::new("Too many attempts, try again later"));
                return (status, [("retry-after", retry_after.to_string())], body).into_response();
            }
            // `{:#}` is the error with its causes, on one line.
            Self::Database(e) => {
                tracing::error!("database error: {e:#}");
                "Internal server error".to_string()
            }
            Self::Internal(e) => {
                tracing::error!("internal error: {e:#}");
                "Internal server error".to_string()
            }
        };
        (status, Json(ErrorBody::new(error))).into_response()
    }
}

/// What cannot be done to a tree, as the API answers it.
impl From<crate::files::FileError> for HttpError {
    fn from(error: crate::files::FileError) -> Self {
        use crate::files::FileError;
        match error {
            FileError::NotFound => HttpError::not_found(),
            FileError::Conflict(message) => HttpError::conflict(message),
            FileError::ReadOnly => HttpError::Status {
                status: StatusCode::CONFLICT,
                message: crate::files::READ_ONLY.into(),
                code: Some(READ_ONLY),
            },
            FileError::Invalid(message) => HttpError::bad_request(message),
            FileError::Unavailable(message) => {
                tracing::warn!("files root unavailable: {message}");
                HttpError::new(
                    StatusCode::SERVICE_UNAVAILABLE,
                    "The files folder is not available right now",
                )
            }
            FileError::TooLarge(max) => HttpError::new(
                StatusCode::PAYLOAD_TOO_LARGE,
                format!(
                    "Too large: at most {} MB here (larger files go through resumable uploads)",
                    max / 1_000_000
                ),
            ),
            FileError::Forbidden(message) => HttpError::new(StatusCode::FORBIDDEN, message),
            // A drive's storage backend fails or is locked: why, fit to show.
            FileError::Down(reason) => HttpError::Status {
                status: StatusCode::SERVICE_UNAVAILABLE,
                message: reason,
                code: Some(STORAGE_UNAVAILABLE),
            },
            FileError::Full => HttpError::new(
                StatusCode::INSUFFICIENT_STORAGE,
                "Not enough room: this would go past your storage limit",
            ),
            FileError::Database(e) => HttpError::Database(e),
            FileError::Internal(e) => HttpError::Internal(e),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    async fn body(error: HttpError) -> (StatusCode, String) {
        let response = error.into_response();
        let status = response.status();
        let bytes = axum::body::to_bytes(response.into_body(), 1024)
            .await
            .unwrap();
        (status, String::from_utf8(bytes.to_vec()).unwrap())
    }

    #[tokio::test]
    async fn a_failure_on_our_side_says_nothing_of_it() {
        let database = HttpError::from(sqlx::Error::Protocol("secret detail".into()));
        let internal = HttpError::internal("saving failed: secret detail");
        for error in [database, internal] {
            let (status, body) = body(error).await;
            assert_eq!(status, StatusCode::INTERNAL_SERVER_ERROR);
            assert_eq!(body, r#"{"error":"Internal server error"}"#);
        }
    }

    #[tokio::test]
    async fn the_step_up_refusals_carry_a_code() {
        for (error, code) in [
            (HttpError::step_up_required(), STEP_UP_REQUIRED),
            (HttpError::second_factor_required(), SECOND_FACTOR_REQUIRED),
        ] {
            let (status, text) = body(error).await;
            assert_eq!(status, StatusCode::FORBIDDEN);
            assert!(text.contains(&format!(r#""code":"{code}""#)), "{text}");
        }
    }

    #[tokio::test]
    async fn a_write_to_a_drive_being_moved_carries_a_code() {
        let (status, text) = body(crate::files::FileError::ReadOnly.into()).await;
        assert_eq!(status, StatusCode::CONFLICT);
        assert!(text.contains(r#""code":"read_only""#), "{text}");
        assert!(text.contains("This drive is being moved"), "{text}");
    }

    #[tokio::test]
    async fn a_client_error_keeps_its_status_and_message() {
        let (status, body) = body(HttpError::conflict("Email already in use")).await;
        assert_eq!(status, StatusCode::CONFLICT);
        assert_eq!(body, r#"{"error":"Email already in use"}"#);
    }
}

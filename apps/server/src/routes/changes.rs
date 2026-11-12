//! The change feed of the sync clients and the live events of the open apps
//! (docs/M1_DESIGN.md §2.3).

use crate::error::{ErrorBody, HttpError};
use crate::files::{Kind, Topic};
use crate::routes::auth::AuthUser;
use crate::state::AppState;
use axum::Json;
use axum::extract::{Query, State};
use axum::http::StatusCode;
use axum::response::sse::{Event, KeepAlive, Sse};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::convert::Infallible;
use std::future::Future;
use std::pin::Pin;
use std::task::{Context, Poll};
use utoipa::{IntoParams, ToSchema};
use utoipa_axum::router::OpenApiRouter;
use utoipa_axum::routes;
use uuid::Uuid;

pub fn router() -> OpenApiRouter<AppState> {
    OpenApiRouter::new()
        .routes(routes!(changes))
        .routes(routes!(events))
}

#[derive(Deserialize, IntoParams)]
#[into_params(parameter_in = Query)]
pub struct ChangesParams {
    /// The `cursor` of the previous answer. Left out: no changes, only the
    /// cursor of now, to start from after a full listing.
    pub cursor: Option<String>,
    /// 1 to 1000; 500 by default.
    pub limit: Option<u32>,
}

/// What happened to a file or folder, and where it is after it.
#[derive(Serialize, ToSchema)]
pub struct Change {
    pub node_id: Uuid,
    /// The drive it is in (a string, a UUID for this server's drives): the
    /// feed covers every drive of the user's, and a placed share is in
    /// their My drive. A drive's root created or deleted (parent null) is
    /// the user joining or leaving that drive.
    #[schema(value_type = String)]
    pub drive_id: Uuid,
    /// `create`, `update` (the content), `move` (a rename or a move) or
    /// `delete` (with what was in a folder).
    pub op: String,
    /// Null for a deletion.
    #[schema(required = true)]
    pub kind: Option<Kind>,
    /// After the change; null for a deletion.
    #[schema(required = true)]
    pub parent_id: Option<Uuid>,
    #[schema(required = true)]
    pub name: Option<String>,
    #[schema(required = true)]
    pub version: Option<i64>,
    /// The encrypted folder it is in now (its `e2e_root`, see `Node`); null
    /// when it is not encrypted, or gone.
    #[schema(required = true)]
    pub e2e_root: Option<Uuid>,
}

/// A page of changes. Several changes to one file in it come as its last.
#[derive(Serialize, ToSchema)]
pub struct Changes {
    pub changes: Vec<Change>,
    /// What to send as `cursor` next time (opaque).
    pub cursor: String,
    /// Whether more changes are waiting: ask again at once.
    pub has_more: bool,
}

#[derive(sqlx::FromRow)]
struct Row {
    seq: i64,
    node_id: Uuid,
    drive_id: Uuid,
    op: String,
    kind: Option<Kind>,
    parent_id: Option<Uuid>,
    name: Option<String>,
    version: Option<i64>,
    e2e_root: Option<Uuid>,
}

#[utoipa::path(
    get,
    path = "/changes",
    tag = "Files",
    operation_id = "listChanges",
    summary = "What changed since a cursor",
    description = "For the sync clients: start with a full listing and the cursor of now (no `cursor`), then ask with the last cursor. The feed covers all of the user's drives, each change with its `drive_id`. The feed keeps 30 days; an older cursor, or one from before the files folder was changed, is answered with 410: list everything again.",
    params(ChangesParams),
    responses(
        (status = 200, description = "The changes since the cursor", body = Changes),
        (status = 400, description = "An invalid cursor", body = ErrorBody),
        (status = 401, description = "Not signed in", body = ErrorBody),
        (status = 410, description = "The cursor is too old: list everything again", body = ErrorBody),
    ),
    security(("session" = []), ("api_key" = [])),
)]
async fn changes(
    State(state): State<AppState>,
    user: AuthUser,
    Query(params): Query<ChangesParams>,
) -> Result<Json<Changes>, HttpError> {
    let user_id = user.0.id;
    let latest: i64 =
        sqlx::query_scalar("SELECT coalesce(max(seq), 0) FROM changes WHERE user_id = ?1")
            .bind(user_id)
            .fetch_one(state.db.reader())
            .await?;
    let (epoch, floor): (i64, i64) =
        sqlx::query_as("SELECT feed_epoch, feed_floor FROM users WHERE id = ?1")
            .bind(user_id)
            .fetch_one(state.db.reader())
            .await?;
    let Some(cursor) = params.cursor else {
        return Ok(Json(Changes {
            changes: Vec::new(),
            cursor: format!("{epoch}.{}", latest.max(floor)),
            has_more: false,
        }));
    };
    let (cursor_epoch, cursor) = cursor
        .split_once('.')
        .and_then(|(e, s)| Some((e.parse::<i64>().ok()?, s.parse::<i64>().ok()?)))
        .ok_or_else(|| HttpError::bad_request("Invalid cursor"))?;
    if cursor_epoch != epoch || cursor < floor {
        return Err(HttpError::new(
            StatusCode::GONE,
            "The cursor is too old: list everything again",
        ));
    }
    let limit = params.limit.unwrap_or(500).clamp(1, 1000) as i64;
    let rows: Vec<Row> = sqlx::query_as(
        "SELECT c.seq, c.node_id, coalesce(c.drive_id, c.user_id) AS drive_id, c.op, c.kind, c.parent_id, c.name, c.version, n.e2e_root
         FROM changes c LEFT JOIN nodes n ON n.id = c.node_id AND c.op != 'delete'
         WHERE c.user_id = ?1 AND c.seq > ?2 ORDER BY c.seq LIMIT ?3",
    )
    .bind(user_id)
    .bind(cursor)
    .bind(limit + 1)
    .fetch_all(state.db.reader())
    .await?;
    let has_more = rows.len() as i64 > limit;
    let rows: Vec<Row> = rows.into_iter().take(limit as usize).collect();
    let next = rows.last().map(|r| r.seq).unwrap_or(cursor);
    // The last change of each node in the page, in the order of those.
    let mut last: HashMap<Uuid, usize> = HashMap::new();
    for (i, row) in rows.iter().enumerate() {
        last.insert(row.node_id, i);
    }
    let changes = rows
        .into_iter()
        .enumerate()
        .filter(|(i, row)| last[&row.node_id] == *i)
        .map(|(_, row)| Change {
            node_id: row.node_id,
            drive_id: row.drive_id,
            op: row.op,
            kind: row.kind,
            parent_id: row.parent_id,
            name: row.name,
            version: row.version,
            e2e_root: row.e2e_root,
        })
        .collect();
    Ok(Json(Changes {
        changes,
        cursor: format!("{epoch}.{next}"),
        has_more,
    }))
}

#[utoipa::path(
    get,
    path = "/events",
    tag = "Files",
    operation_id = "streamEvents",
    summary = "Live events",
    description = "Server-sent events for the open apps: `changes` whenever the change feed has new rows (then ask `/changes`), `drop-box` whenever an item of the drop box changed (then ask `/drop-box/items`), and a comment every 30 seconds to keep the connection open.",
    responses(
        (status = 200, description = "The event stream", content_type = "text/event-stream"),
        (status = 401, description = "Not signed in", body = ErrorBody),
    ),
    security(("session" = []), ("api_key" = [])),
)]
async fn events(
    State(state): State<AppState>,
    user: AuthUser,
) -> impl axum::response::IntoResponse {
    Sse::new(Events {
        user_id: user.0.id,
        receiver: Some(state.files.subscribe()),
        waiting: None,
        lagged: false,
    })
    .keep_alive(KeepAlive::new().interval(std::time::Duration::from_secs(30)))
}

type Received = (
    tokio::sync::broadcast::Receiver<(Uuid, Topic)>,
    Result<(Uuid, Topic), tokio::sync::broadcast::error::RecvError>,
);

/// The events of one user, from the service's broadcast of every user's.
/// A stream by hand: the receiver goes into the future that waits on it and
/// comes back out with what it received.
pub struct Events {
    user_id: Uuid,
    receiver: Option<tokio::sync::broadcast::Receiver<(Uuid, Topic)>>,
    waiting: Option<Pin<Box<dyn Future<Output = Received> + Send>>>,
    /// Events were missed: a `drop-box` event is owed after the `changes`
    /// one.
    lagged: bool,
}

impl futures_core::Stream for Events {
    type Item = Result<Event, Infallible>;

    fn poll_next(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        if self.lagged {
            self.lagged = false;
            return Poll::Ready(Some(Ok(Event::default()
                .event(Topic::DropBox.event())
                .data("{}"))));
        }
        loop {
            if self.waiting.is_none() {
                let Some(mut receiver) = self.receiver.take() else {
                    return Poll::Ready(None);
                };
                self.waiting = Some(Box::pin(async move {
                    let received = receiver.recv().await;
                    (receiver, received)
                }));
            }
            let (receiver, received) = match self.waiting.as_mut().unwrap().as_mut().poll(cx) {
                Poll::Pending => return Poll::Pending,
                Poll::Ready(result) => result,
            };
            self.waiting = None;
            self.receiver = Some(receiver);
            match received {
                Ok((_, Topic::Reindex)) => continue,
                Ok((user, topic)) if user == self.user_id => {
                    return Poll::Ready(Some(Ok(Event::default().event(topic.event()).data("{}"))));
                }
                Ok(_) => continue,
                // Missed some: say both may have changed; the feed and the
                // drop box have it all.
                Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => {
                    self.lagged = true;
                    return Poll::Ready(Some(Ok(Event::default().event("changes").data("{}"))));
                }
                Err(tokio::sync::broadcast::error::RecvError::Closed) => {
                    return Poll::Ready(None);
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use futures_core::Stream;

    /// The next item of a stream, or None if it has none ready yet.
    async fn next(events: &mut Events) -> Option<Option<Result<Event, Infallible>>> {
        let mut events = Pin::new(events);
        tokio::time::timeout(
            std::time::Duration::from_millis(200),
            std::future::poll_fn(|cx| events.as_mut().poll_next(cx)),
        )
        .await
        .ok()
    }

    #[tokio::test]
    async fn a_user_gets_the_events_of_their_own_feed_only() {
        let db = crate::db::open(":memory:").await.unwrap();
        let files = crate::files::Files::new(
            db,
            std::env::temp_dir(),
            crate::config::StorageUrl::Local(std::env::temp_dir()),
        );
        let (me, someone) = (Uuid::now_v7(), Uuid::now_v7());
        let mut events = Events {
            user_id: me,
            receiver: Some(files.subscribe()),
            waiting: None,
            lagged: false,
        };
        files.notify(someone);
        assert!(next(&mut events).await.is_none(), "another user's change");
        files.notify(me);
        assert!(matches!(next(&mut events).await, Some(Some(Ok(_)))));
        assert!(next(&mut events).await.is_none(), "nothing more");
    }

    #[tokio::test]
    async fn drop_box_events_have_their_own_name_and_follow_a_lag() {
        let db = crate::db::open(":memory:").await.unwrap();
        let files = crate::files::Files::new(
            db,
            std::env::temp_dir(),
            crate::config::StorageUrl::Local(std::env::temp_dir()),
        );
        let me = Uuid::now_v7();
        let mut events = Events {
            user_id: me,
            receiver: Some(files.subscribe()),
            waiting: None,
            lagged: false,
        };
        let name = |e: Option<Option<Result<Event, Infallible>>>| {
            // The event's wire form starts with its name.
            let event = e.unwrap().unwrap().unwrap();
            format!("{event:?}")
        };
        files.announce(me, Topic::DropBox);
        assert!(name(next(&mut events).await).contains("drop-box"));
        // More than the channel holds: both kinds are owed.
        for _ in 0..100 {
            files.announce(Uuid::now_v7(), Topic::Changes);
        }
        assert!(name(next(&mut events).await).contains("changes"));
        assert!(name(next(&mut events).await).contains("drop-box"));
    }
}

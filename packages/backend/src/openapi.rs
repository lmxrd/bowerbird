//! The OpenAPI description of the API, written to a file by the `openapi`
//! binary (`mise run gen:spec`), and served only with `SWAGGER_UI` set: the
//! frontend's client is generated from it. The paths come from the router
//! itself: every handler is registered with `routes!` next to its
//! `#[utoipa::path]` (see `routes/`), so a routed endpoint is documented and
//! a documented one is routed. This file holds the rest: the title, the
//! tags and the security schemes.

use crate::auth::COOKIE;
use utoipa::openapi::security::{ApiKey, ApiKeyValue, HttpAuthScheme, HttpBuilder, SecurityScheme};
use utoipa::{Modify, OpenApi};

#[derive(OpenApi)]
#[openapi(
    info(
        title = "Organizer API",
        description = "The API of Organizer, a light, self-hosted file server whose drop box \
            files things for you. Endpoints need the session cookie from /api/auth/login (or \
            /api/auth/setup), or a token as `Authorization: Bearer`: an API key or a device's.",
    ),
    tags(
        (name = "Auth", description = "Signing in and out, and the first-run setup"),
        (name = "Account", description = "The signed-in user's own email, password, two-factor authentication and tokens"),
        (name = "Superadmin", description = "Settings of the whole instance, such as single sign-on, where the files are, the AI connection and the backups: superadmin only, a session whose second factor was confirmed within 15 minutes, never a token"),
        (name = "Drives", description = "Your drives (My drive and the shared drives you are a member of), who has access to a shared drive, and a drive's settings"),
        (name = "Teams", description = "Your teams: their members and managers, their shared drives and their settings"),
        (name = "Storage", description = "Your own S3-compatible storage, where a new drive may be stored, and unlocking private credentials for a session"),
        (name = "Mirrors", description = "Your mirrors: read-only copies of your drives in your own storage, when they copy, ejecting one as a drive, and restoring from one into its drive"),
        (name = "Settings", description = "Your settings with levels: what applies to you, where it comes from, and whether you may change it"),
        (name = "Files", description = "The files and folders, the trash, the change feed and the live events"),
        (name = "Sharing", description = "Who has access to a file or folder (roles for people and teams, limited access, its owner), and what others shared with you"),
        (name = "Encryption", description = "End-to-end-encrypted folders: your keys and the folders' keys, wrapped by your devices"),
        (name = "Federation", description = "People on other servers (Open Cloud Mesh): your contacts there, the invites that make them, and looking an address up before sharing with it"),
        (name = "Links", description = "The pages of public links: what anyone with a link may see and do, without signing in"),
        (name = "Drop box", description = "The AI drop box: its settings and sensitive-file rules"),
        (name = "Notifications", description = "Push notifications: what you are told about, your devices, and what is new"),
        (name = "Search", description = "Full-text search over names, paths and the text of files"),
        (name = "Backups", description = "Your files in the server's backups: browse a snapshot, restore a file or folder"),
        (name = "Uploads", description = "Resumable uploads (draft-ietf-httpbis-resumable-upload, interop version 6)"),
        (name = "System", description = "Operations"),
    ),
    modifiers(&Security),
)]
pub struct ApiDoc;

/// Declares the session cookie and the bearer tokens endpoints are secured
/// with.
struct Security;

impl Modify for Security {
    fn modify(&self, openapi: &mut utoipa::openapi::OpenApi) {
        let components = openapi.components.get_or_insert_with(Default::default);
        components.add_security_scheme(
            "session",
            SecurityScheme::ApiKey(ApiKey::Cookie(ApiKeyValue::new(COOKIE))),
        );
        // API keys (`org_...`) made in the settings, and the apps' device
        // tokens: on the endpoints that list it, capped by the token's scope.
        components.add_security_scheme(
            "api_key",
            SecurityScheme::Http(
                HttpBuilder::new()
                    .scheme(HttpAuthScheme::Bearer)
                    .description(Some(
                        "An API key or a device token: `read` ones may only GET; none reaches a superadmin endpoint (those take a session).",
                    ))
                    .build(),
            ),
        );
    }
}

/// The document without paths: what the router starts from.
pub fn base() -> utoipa::openapi::OpenApi {
    ApiDoc::openapi()
}

/// The complete document: the base plus every routed endpoint.
pub fn spec() -> utoipa::openapi::OpenApi {
    crate::api().split_for_parts().1
}

/// The spec as pretty printed JSON.
pub fn spec_json() -> String {
    spec().to_pretty_json().expect("the spec serializes")
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeSet;

    fn operations() -> Vec<(String, String, serde_json::Value)> {
        let spec: serde_json::Value = serde_json::from_str(&spec_json()).unwrap();
        let mut found = Vec::new();
        for (path, item) in spec["paths"].as_object().unwrap() {
            for (method, operation) in item.as_object().unwrap() {
                found.push((method.to_uppercase(), path.clone(), operation.clone()));
            }
        }
        found
    }

    #[test]
    fn operations_have_unique_ids_tags_and_summaries() {
        let operations = operations();
        assert!(!operations.is_empty());
        let ids: BTreeSet<_> = operations
            .iter()
            .map(|(_, _, op)| op["operationId"].as_str().expect("an operationId"))
            .collect();
        assert_eq!(ids.len(), operations.len(), "operation ids must be unique");
        for (method, path, op) in &operations {
            assert!(op["tags"][0].is_string(), "{method} {path} has no tag");
            assert!(op["summary"].is_string(), "{method} {path} has no summary");
        }
    }

    #[test]
    fn only_auth_system_and_link_page_endpoints_are_public() {
        for (method, path, op) in operations() {
            // "Links": the public link pages, which the link's token (and
            // its password) open without an account.
            let may_be_public =
                ["Auth", "System", "Links"].contains(&op["tags"][0].as_str().unwrap());
            let secured = op["security"].is_array();
            assert!(may_be_public || secured, "{method} {path} is not secured");
        }
    }

    #[test]
    fn path_parameters_are_in_the_path() {
        for (method, path, op) in operations() {
            for param in op["parameters"].as_array().into_iter().flatten() {
                if param["in"] == "path" {
                    let name = param["name"].as_str().unwrap();
                    assert!(
                        path.contains(&format!("{{{name}}}")),
                        "{method} {path}: `{name}` is not in the path (a query parameter needs #[into_params(parameter_in = Query)])"
                    );
                }
            }
        }
    }

    #[test]
    fn every_path_is_under_api() {
        for (method, path, _) in operations() {
            assert!(path.starts_with("/api/"), "{method} {path}");
        }
    }
}

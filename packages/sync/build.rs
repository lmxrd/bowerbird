//! The API client, generated with progenitor from the backend's OpenAPI
//! spec (`mise run gen:spec` writes it) into OUT_DIR, never committed.
//!
//! progenitor reads OpenAPI 3.0 and utoipa writes 3.1, so the spec is
//! brought down to 3.0 on the way (the spec itself is left as it is):
//!
//! - `type: [T, "null"]` becomes `type: T, nullable: true`;
//! - a nullable reference (`oneOf: [{$ref}, {type: null}]`, an `Option` of
//!   a schema) becomes the reference itself, out of `required`, which the
//!   generated type makes an `Option` anyway (as the iOS app's spec.sh does);
//! - only the operations the engine calls are kept: the others (multipart
//!   forms, the resumable uploads' headers, the event stream) are done by
//!   hand on the same reqwest client, as progenitor cannot describe them.

use serde_json::{Map, Value};
use std::path::PathBuf;

/// The operations generated, by their `operationId`.
const OPERATIONS: &[&str] = &[
    "getSession",
    "deviceToken",
    "signOutDevice",
    "getRoot",
    "listDrives",
    "getDrive",
    "getFile",
    "getFileByPath",
    "listChildren",
    "listChanges",
    "createFolder",
    "moveFile",
    "deleteFile",
    "listTrash",
    "listDropBoxItems",
    "getEncryption",
    "getFolderKeys",
    "addFolderKeys",
    "listEncryptionTasks",
];

fn main() {
    let spec = std::env::var("OPENAPI_SPEC")
        .map(PathBuf::from)
        .unwrap_or_else(|_| {
            PathBuf::from(std::env::var("CARGO_MANIFEST_DIR").unwrap())
                .join("../backend/target/specs/openapi.json")
        });
    println!("cargo:rerun-if-changed={}", spec.display());
    println!("cargo:rerun-if-env-changed=OPENAPI_SPEC");
    let text = std::fs::read_to_string(&spec).unwrap_or_else(|e| {
        panic!(
            "cannot read the OpenAPI spec at {}: {e}. Run `mise run gen:spec` first.",
            spec.display()
        )
    });
    let mut spec: Value = serde_json::from_str(&text).expect("the spec is JSON");
    keep_operations(&mut spec);
    spec["openapi"] = Value::String("3.0.3".into());
    unrequire_nullable_refs(&mut spec);
    to_openapi_30(&mut spec);

    let spec = serde_json::from_value(spec).expect("an OpenAPI 3.0 document");
    let mut generator = progenitor::Generator::default();
    let tokens = generator
        .generate_tokens(&spec)
        .expect("progenitor generates the client");
    let ast = syn::parse2(tokens).expect("the generated code parses");
    let out = PathBuf::from(std::env::var("OUT_DIR").unwrap()).join("client.rs");
    std::fs::write(out, prettyplease::unparse(&ast)).expect("the client is written");
}

/// Only the operations in `OPERATIONS`; a path left without any goes.
fn keep_operations(spec: &mut Value) {
    let Some(paths) = spec["paths"].as_object_mut() else {
        return;
    };
    for item in paths.values_mut() {
        if let Some(item) = item.as_object_mut() {
            item.retain(|_, op| {
                op.get("operationId")
                    .and_then(Value::as_str)
                    .is_none_or(|id| OPERATIONS.contains(&id))
                    && op.is_object()
            });
        }
    }
    paths.retain(|_, item| item.as_object().is_some_and(|i| !i.is_empty()));
}

fn is_nullable_one_of(value: &Value) -> bool {
    value["oneOf"]
        .as_array()
        .is_some_and(|all| all.len() == 2 && all.iter().any(is_null_schema))
}

fn is_null_schema(value: &Value) -> bool {
    value.as_object().is_some_and(|o| o.len() == 1) && value["type"] == "null"
}

/// A nullable reference becomes the reference, and its property optional.
fn unrequire_nullable_refs(value: &mut Value) {
    match value {
        Value::Object(object) => {
            let nullable: Vec<String> = object
                .get("properties")
                .and_then(Value::as_object)
                .map(|p| {
                    p.iter()
                        .filter(|(_, v)| is_nullable_one_of(v))
                        .map(|(k, _)| k.clone())
                        .collect()
                })
                .unwrap_or_default();
            if let Some(Value::Array(required)) = object.get_mut("required") {
                required.retain(|r| !nullable.iter().any(|n| r == n.as_str()));
            }
            if is_nullable_one_of(&Value::Object(object.clone())) {
                let inner = object["oneOf"]
                    .as_array()
                    .and_then(|all| all.iter().find(|v| !is_null_schema(v)).cloned())
                    .unwrap_or(Value::Null);
                let description = object.get("description").cloned();
                *object = match inner {
                    Value::Object(inner) => inner,
                    _ => Map::new(),
                };
                if let Some(description) = description {
                    object.insert("description".into(), description);
                }
            }
            for v in object.values_mut() {
                unrequire_nullable_refs(v);
            }
        }
        Value::Array(items) => items.iter_mut().for_each(unrequire_nullable_refs),
        _ => {}
    }
}

/// The 3.1 forms that 3.0 does not have.
fn to_openapi_30(value: &mut Value) {
    match value {
        Value::Object(object) => {
            if let Some(Value::Array(types)) = object.get("type").cloned() {
                let others: Vec<Value> = types.iter().filter(|t| *t != "null").cloned().collect();
                if others.len() == 1 {
                    object.insert("type".into(), others[0].clone());
                    if others.len() < types.len() {
                        object.insert("nullable".into(), Value::Bool(true));
                    }
                }
            }
            // A reference with siblings is a 3.1 form: 3.0 ignores them, and
            // progenitor would not take the reference.
            if object.contains_key("$ref") {
                object.retain(|k, _| k == "$ref");
            }
            object.remove("contentMediaType");
            object.remove("examples");
            for v in object.values_mut() {
                to_openapi_30(v);
            }
        }
        Value::Array(items) => items.iter_mut().for_each(to_openapi_30),
        _ => {}
    }
}

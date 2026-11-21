//! The agent API as one table. Each [`Op`] is one route of `/api/agent` and
//! one MCP tool, with the same name, input and answer: the HTTP router is
//! built from [`OPS`], `hoja serve --socket` offers them as `hoja__<name>`,
//! and `hoja mcp` forwards them to a running server. Both front ends end in
//! [`run`], which calls the same store functions as the person's handlers,
//! so an agent meets the same gates whichever way it comes in.
//!
//! What only the person decides (marking content reviewed, approving a
//! document, deciding cards, accepting type and template files, deleting
//! records) has no agent route and so no tool; `tests/parity.rs` lists each
//! of those person routes with the reason it is not a tool.

use axum::http::{Method, StatusCode};
use axum::response::Response;
use schemars::{JsonSchema, schema_for};
use serde::de::DeserializeOwned;
use serde_json::{Map, Value, json};

use crate::gates::Actor;
use crate::http::{self, App};
use crate::model::{
    BuildDocument, CardStatus, CreateDocument, CreateRecord, Empty, ListDocuments, ListRecords,
    SelectSection, UpdateDocument, UpdateRecord,
};

/// The HTTP method an operation is served on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Verb {
    /// Input from the path and the query string.
    Get,
    /// Input from the path and a JSON body.
    Post,
}

impl Verb {
    pub fn method(self) -> Method {
        match self {
            Self::Get => Method::GET,
            Self::Post => Method::POST,
        }
    }
}

pub struct Op {
    /// The tool's name, offered by a Tracon node as `hoja__<name>`.
    pub name: &'static str,
    pub verb: Verb,
    /// The route under `/api/agent`. Each `{param}` is also a required
    /// string argument of the tool.
    pub path: &'static str,
    /// Changes something. Tracon shows it as the tool's hint; hoja's own
    /// gates decide what an agent's write does.
    pub writes: bool,
    pub description: &'static str,
    /// The schema of the rest of the input: the query of a GET, the body of
    /// a POST.
    input: fn() -> Value,
}

fn schema<T: JsonSchema>() -> Value {
    serde_json::to_value(schema_for!(T)).unwrap_or_else(|_| json!({}))
}

macro_rules! op {
    ($name:literal, $verb:ident $path:literal, $input:ty, $writes:literal, $description:literal) => {
        Op {
            name: $name,
            verb: Verb::$verb,
            path: $path,
            writes: $writes,
            description: $description,
            input: schema::<$input>,
        }
    };
}

pub const OPS: &[Op] = &[
    op!(
        "types",
        Get "/types",
        Empty,
        false,
        "List the accepted document types: their record kinds, fields, which fields only the \
         person may write, sections and lints. Read this before writing records or documents."
    ),
    op!(
        "records",
        Get "/records",
        ListRecords,
        false,
        "List records, optionally of one type and kind."
    ),
    op!(
        "record_show",
        Get "/records/{id}",
        Empty,
        false,
        "Read a record and its revisions: who changed what, and when. Its version is what \
         record_edit needs."
    ),
    op!(
        "record_create",
        Post "/records",
        CreateRecord,
        true,
        "Create a record. Where the type lets agents write directly it is created at once \
         (state applied); otherwise it becomes a card the person accepts or rejects (state \
         awaiting_person, with an approval_id for card_status)."
    ),
    op!(
        "record_edit",
        Post "/records/{id}",
        UpdateRecord,
        true,
        "Change fields of a record you read at `version`. Applied at once where the type \
         allows it; otherwise a card for the person, which goes stale if the record moves \
         before they decide. Fields only the person may write are refused."
    ),
    op!(
        "documents",
        Get "/documents",
        ListDocuments,
        false,
        "List documents, optionally of one type."
    ),
    op!(
        "document_show",
        Get "/documents/{id}",
        Empty,
        false,
        "Read a document as it resolves: its fields and selected records, the content hash, \
         lint findings (and whether any block approval) and the exports its type offers."
    ),
    op!(
        "document_create",
        Post "/documents",
        CreateDocument,
        true,
        "Create a document. Applied at once or held as a card for the person, as the type says."
    ),
    op!(
        "document_edit",
        Post "/documents/{id}",
        UpdateDocument,
        true,
        "Change a document's title or fields, against the `version` you read. Applied at once \
         or held as a card for the person, as the type says."
    ),
    op!(
        "document_select",
        Post "/documents/{id}/sections/{section}",
        SelectSection,
        true,
        "Set a section's records, in order, against the document `version` you read. Records \
         left out are omitted."
    ),
    op!(
        "document_build",
        Post "/documents/{id}/build",
        BuildDocument,
        true,
        "Render a document into the workspace's out/ directory and return the path. A draft \
         is named as a draft; only the person approves."
    ),
    op!(
        "card_status",
        Get "/approvals/{approval_id}",
        CardStatus,
        false,
        "Wait up to `wait` seconds (at most 45) for the person to decide a card. \
         still_waiting means call again with the same id; accepted, rejected and \
         changes_requested carry the person's reason, and stale means the target moved \
         since you read it."
    ),
];

pub fn find(name: &str) -> Option<&'static Op> {
    OPS.iter().find(|op| op.name == name)
}

impl Op {
    /// The names of the path's `{param}`s, in order.
    pub fn path_params(&self) -> Vec<&'static str> {
        self.path
            .split('/')
            .filter_map(|s| s.strip_prefix('{')?.strip_suffix('}'))
            .collect()
    }

    /// The route as served: `/api/agent<path>`.
    pub fn route(&self) -> String {
        format!("/api/agent{}", self.path)
    }

    /// `read` or `write`, as the plugin manifest says it.
    pub fn hint(&self) -> &'static str {
        if self.writes { "write" } else { "read" }
    }

    /// The tool's input schema: the path's params, then the query or body.
    pub fn input_schema(&self) -> Value {
        let mut schema = (self.input)();
        let Some(object) = schema.as_object_mut() else {
            return json!({"type": "object"});
        };
        object.shift_remove("$schema");
        object.shift_remove("title");
        object.insert("type".into(), json!("object"));
        let rest = object
            .remove("properties")
            .and_then(|p| p.as_object().cloned())
            .unwrap_or_default();
        let mut properties = Map::new();
        let mut required: Vec<Value> = Vec::new();
        for param in self.path_params() {
            properties.insert(
                param.into(),
                json!({"type": "string", "description": format!("The {} in the path.", param.replace('_', " "))}),
            );
            required.push(json!(param));
        }
        properties.extend(rest);
        if let Some(Value::Array(more)) = object.shift_remove("required") {
            required.extend(more);
        }
        object.insert("properties".into(), Value::Object(properties));
        if !required.is_empty() {
            object.insert("required".into(), Value::Array(required));
        }
        schema
    }

    /// The tool as `tools/list` gives it.
    pub fn tool(&self) -> Value {
        json!({
            "name": self.name,
            "description": self.description,
            "inputSchema": self.input_schema(),
            "annotations": {"readOnlyHint": !self.writes, "openWorldHint": false},
        })
    }

    /// Split a tool's arguments into the path's params and the rest.
    pub fn split(&self, args: Value) -> Result<(Vec<String>, Map<String, Value>), String> {
        let mut args = match args {
            Value::Null => Map::new(),
            Value::Object(map) => map,
            _ => return Err("arguments must be a JSON object".into()),
        };
        let mut params = Vec::new();
        for param in self.path_params() {
            match args.shift_remove(param) {
                Some(Value::String(s)) if !s.is_empty() => params.push(s),
                _ => return Err(format!("`{param}` is required: a non-empty string")),
            }
        }
        Ok((params, args))
    }
}

/// Every tool, as `tools/list` gives them.
pub fn tools() -> Vec<Value> {
    OPS.iter().map(Op::tool).collect()
}

// A ready `Response` is the error, as in the handlers.
#[allow(clippy::result_large_err)]
fn input<T: DeserializeOwned>(rest: Map<String, Value>) -> Result<T, Response> {
    serde_json::from_value(Value::Object(rest)).map_err(|e| {
        http::error(
            StatusCode::BAD_REQUEST,
            "invalid_request",
            &format!("Invalid input: {e}"),
        )
    })
}

/// Run `op` as `actor` with the tool's arguments: the path's params and the
/// query or body, in one object. The answer is the agent API's, status and
/// all.
pub async fn run(app: &App, actor: Actor, op: &Op, args: Value) -> Response {
    let (params, rest) = match op.split(args) {
        Ok(split) => split,
        Err(message) => return http::error(StatusCode::BAD_REQUEST, "invalid_request", &message),
    };
    let param = |i: usize| params[i].clone();
    let app = app.clone();
    macro_rules! with_input {
        ($ty:ty, |$v:ident| $body:expr) => {
            match input::<$ty>(rest) {
                Ok($v) => $body,
                Err(response) => response,
            }
        };
    }
    match op.name {
        "types" => with_input!(Empty, |_x| http::agent_types(app).await),
        "records" => with_input!(ListRecords, |q| http::agent_records(app, q).await),
        "record_show" => with_input!(Empty, |_x| http::agent_record(app, param(0)).await),
        "record_create" => with_input!(CreateRecord, |body| {
            http::run_outcome(move || app.store.create_record(&actor, body)).await
        }),
        "record_edit" => with_input!(UpdateRecord, |body| {
            let id = param(0);
            http::run_outcome(move || app.store.update_record(&actor, &id, body)).await
        }),
        "documents" => with_input!(ListDocuments, |q| http::agent_documents(app, q).await),
        "document_show" => with_input!(Empty, |_x| http::agent_document(app, param(0)).await),
        "document_create" => with_input!(CreateDocument, |body| {
            http::run_outcome(move || app.store.create_document(&actor, body)).await
        }),
        "document_edit" => with_input!(UpdateDocument, |body| {
            let id = param(0);
            http::run_outcome(move || app.store.update_document(&actor, &id, body)).await
        }),
        "document_select" => with_input!(SelectSection, |body| {
            let (id, section) = (param(0), param(1));
            http::run_outcome(move || app.store.select(&actor, &id, &section, body)).await
        }),
        "document_build" => with_input!(BuildDocument, |body| {
            http::agent_build(app, param(0), body.format).await
        }),
        "card_status" => with_input!(CardStatus, |q| http::agent_wait(app, param(0), q.wait)
            .await),
        other => http::error(
            StatusCode::NOT_FOUND,
            "not_found",
            &format!("hoja has no operation {other}"),
        ),
    }
}

/// A response's status and JSON body (`null` when it has none).
pub async fn answer(response: Response) -> (StatusCode, Value) {
    let status = response.status();
    let bytes = axum::body::to_bytes(response.into_body(), 16 << 20)
        .await
        .unwrap_or_default();
    (
        status,
        serde_json::from_slice(&bytes).unwrap_or(Value::Null),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn path_params_are_required_string_arguments() {
        let select = find("document_select").unwrap();
        assert_eq!(select.path_params(), ["id", "section"]);
        let schema = select.input_schema();
        assert_eq!(
            schema["required"],
            json!(["id", "section", "version", "record_ids"])
        );
        assert_eq!(schema["properties"]["note"]["x-tracon-format"], "markdown");
        assert_eq!(schema["type"], "object");
        assert!(schema.get("$schema").is_none());

        let (params, rest) = select
            .split(json!({"id": "d1", "section": "done", "version": 2, "record_ids": []}))
            .unwrap();
        assert_eq!(params, ["d1", "done"]);
        assert_eq!(rest.keys().collect::<Vec<_>>(), ["version", "record_ids"]);
        assert!(select.split(json!({"id": "d1"})).is_err());
        assert!(select.split(json!({"id": 1, "section": "x"})).is_err());
        assert!(select.split(json!([])).is_err());
    }

    #[test]
    fn names_are_unique_and_fit_the_node() {
        let mut names: Vec<&str> = OPS.iter().map(|op| op.name).collect();
        names.sort();
        names.dedup();
        assert_eq!(names.len(), OPS.len());
        for name in names {
            assert!(format!("hoja__{name}").len() <= 64);
            assert!(!name.contains("__"));
        }
    }
}

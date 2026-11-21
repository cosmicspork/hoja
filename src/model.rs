use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

use crate::config::TypeConfig;
use crate::gates::Author;

#[derive(Debug, Clone, Serialize)]
pub struct Attribution {
    pub author: Author,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub agent: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct Record {
    pub id: String,
    #[serde(rename = "type")]
    pub type_name: String,
    pub kind: String,
    pub fields: Map<String, Value>,
    pub reviewed: bool,
    pub reviewed_at: Option<String>,
    pub version: i64,
    pub created_at: String,
    pub updated_at: String,
    pub deleted: bool,
    pub created_by: Option<Attribution>,
    pub updated_by: Option<Attribution>,
}

#[derive(Debug, Clone, Serialize)]
pub struct Section {
    pub name: String,
    pub record_ids: Vec<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct Document {
    pub id: String,
    #[serde(rename = "type")]
    pub type_name: String,
    pub title: String,
    pub fields: Map<String, Value>,
    pub sections: Vec<Section>,
    /// The person approved exactly the current content.
    pub approved: bool,
    /// When the person last approved this document, whatever changed since.
    pub approved_at: Option<String>,
    pub version: i64,
    pub created_at: String,
    pub updated_at: String,
    pub created_by: Option<Attribution>,
    pub updated_by: Option<Attribution>,
}

#[derive(Debug, Clone, Serialize)]
pub struct Revision {
    pub version: i64,
    pub author: Author,
    pub agent: Option<String>,
    pub approval_id: Option<String>,
    pub edited: bool,
    pub change: String,
    pub snapshot: Value,
    pub created_at: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Action {
    #[serde(rename = "record.create")]
    RecordCreate,
    #[serde(rename = "record.update")]
    RecordUpdate,
    #[serde(rename = "document.create")]
    DocumentCreate,
    #[serde(rename = "document.update")]
    DocumentUpdate,
    #[serde(rename = "document.select")]
    DocumentSelect,
}

impl Action {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::RecordCreate => "record.create",
            Self::RecordUpdate => "record.update",
            Self::DocumentCreate => "document.create",
            Self::DocumentUpdate => "document.update",
            Self::DocumentSelect => "document.select",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        [
            Self::RecordCreate,
            Self::RecordUpdate,
            Self::DocumentCreate,
            Self::DocumentUpdate,
            Self::DocumentSelect,
        ]
        .into_iter()
        .find(|a| a.as_str() == s)
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct Approval {
    pub id: String,
    pub action: Action,
    #[serde(rename = "type")]
    pub type_name: String,
    pub target_id: Option<String>,
    pub base_version: Option<i64>,
    pub payload: Value,
    pub agent: String,
    pub note: String,
    /// `pending`, `accepted`, `rejected` or `changes_requested`.
    pub state: String,
    pub reason: Option<String>,
    pub edited: bool,
    pub result: Option<Value>,
    pub created_at: String,
    pub decided_at: Option<String>,
    /// The target changed underneath this card since the agent read it.
    pub stale: bool,
    /// The target as it is now, for comparison; null for creates.
    pub current: Value,
}

/// What an agent's write turned into.
#[derive(Debug, Clone, Serialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum Outcome<T> {
    Applied { result: T },
    AwaitingPerson { approval_id: String },
}

#[derive(Debug, Clone, Serialize)]
pub struct TypeStatus {
    pub name: String,
    /// `accepted`, `new`, `changed`, `missing` (file removed; the accepted copy still applies).
    pub state: String,
    pub file_hash: Option<String>,
    pub accepted_hash: Option<String>,
    pub accepted_at: Option<String>,
    /// Why the file on disk can't be accepted as it is.
    pub error: Option<String>,
    pub config: Option<TypeConfig>,
}

#[derive(Debug, Clone, Serialize)]
pub struct TypeChange {
    pub name: String,
    pub file_hash: Option<String>,
    pub file_body: Option<String>,
    pub accepted_body: Option<String>,
    pub error: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct TemplateStatus {
    /// Relative to `templates/`.
    pub path: String,
    /// `accepted`, `new`, `changed`, `missing` (file removed; the accepted copy still applies).
    pub state: String,
    pub file_hash: Option<String>,
    pub accepted_hash: Option<String>,
    pub accepted_at: Option<String>,
    pub error: Option<String>,
    /// Accepted types whose template reaches this file.
    pub used_by: Vec<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct TemplateChange {
    pub path: String,
    pub file_hash: Option<String>,
    /// The file's text, when it is UTF-8.
    pub file_text: Option<String>,
    pub accepted_text: Option<String>,
    pub binary: bool,
    pub file_size: Option<usize>,
    pub accepted_size: Option<usize>,
    pub error: Option<String>,
    pub used_by: Vec<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct LintFinding {
    pub level: crate::config::Level,
    pub message: String,
    pub record_id: Option<String>,
    pub field: Option<String>,
}

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct CreateRecord {
    /// The accepted document type the record belongs to.
    #[serde(rename = "type")]
    pub type_name: String,
    /// The record kind, one the type declares.
    pub kind: String,
    /// Field values by name, as the kind declares them.
    #[serde(default)]
    pub fields: Map<String, Value>,
    /// Why, shown to the person on any card this opens.
    #[serde(default)]
    #[schemars(extend("x-tracon-format" = "markdown"))]
    pub note: String,
}

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct UpdateRecord {
    /// The version you read. A change against an older one is refused, and a
    /// card opened against it goes stale if the record moves.
    pub version: i64,
    /// A patch: only the named fields change; null clears one.
    pub fields: Map<String, Value>,
    /// Why, shown to the person on any card this opens.
    #[serde(default)]
    #[schemars(extend("x-tracon-format" = "markdown"))]
    pub note: String,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SetReviewed {
    pub version: i64,
    pub reviewed: bool,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct VersionOnly {
    pub version: i64,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Approve {
    pub version: i64,
    /// The content hash of what the person was shown.
    pub hash: String,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AcceptTemplate {
    pub path: String,
    pub hash: String,
}

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct CreateDocument {
    /// The accepted document type.
    #[serde(rename = "type")]
    pub type_name: String,
    pub title: String,
    /// Document field values by name, as the type declares them.
    #[serde(default)]
    pub fields: Map<String, Value>,
    /// Why, shown to the person on any card this opens.
    #[serde(default)]
    #[schemars(extend("x-tracon-format" = "markdown"))]
    pub note: String,
}

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct UpdateDocument {
    /// The version you read. A change against an older one is refused, and a
    /// card opened against it goes stale if the document moves.
    pub version: i64,
    /// A new title; omit to keep it.
    #[serde(default)]
    pub title: Option<String>,
    /// A patch: only the named fields change; null clears one.
    #[serde(default)]
    pub fields: Map<String, Value>,
    /// Why, shown to the person on any card this opens.
    #[serde(default)]
    #[schemars(extend("x-tracon-format" = "markdown"))]
    pub note: String,
}

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct SelectSection {
    /// The document version you read.
    pub version: i64,
    /// The section's records, in order. Records left out are omitted; an
    /// empty list clears the section.
    pub record_ids: Vec<String>,
    /// Why, shown to the person on any card this opens.
    #[serde(default)]
    #[schemars(extend("x-tracon-format" = "markdown"))]
    pub note: String,
}

/// No input.
#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Empty {}

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ListRecords {
    /// Only records of this document type.
    #[serde(rename = "type", default)]
    pub type_name: Option<String>,
    /// Only records of this kind.
    #[serde(default)]
    pub kind: Option<String>,
}

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ListDocuments {
    /// Only documents of this type.
    #[serde(rename = "type", default)]
    pub type_name: Option<String>,
}

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct BuildDocument {
    /// The export to write, one the type lists in `exports`.
    pub format: crate::config::Format,
}

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct CardStatus {
    /// Seconds to wait for the person's decision, at most 45; 0 answers at
    /// once. Over HTTP this is the `wait` query parameter.
    #[serde(default, deserialize_with = "whole_seconds")]
    #[schemars(with = "Option<u64>")]
    pub wait: Option<u64>,
}

/// A count of seconds, as a JSON integer or, from a query string, digits.
fn whole_seconds<'de, D: serde::Deserializer<'de>>(d: D) -> Result<Option<u64>, D::Error> {
    #[derive(Deserialize)]
    #[serde(untagged)]
    enum Seconds {
        Number(u64),
        Text(String),
    }
    match Option::<Seconds>::deserialize(d)? {
        None => Ok(None),
        Some(Seconds::Number(n)) => Ok(Some(n)),
        Some(Seconds::Text(t)) => t
            .parse()
            .map(Some)
            .map_err(|_| serde::de::Error::custom("wait must be a whole number of seconds")),
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Decision {
    Accept,
    Reject,
    RequestChanges,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Decide {
    pub decision: Decision,
    #[serde(default)]
    pub reason: Option<String>,
    /// The person's edit of the card's payload, accepted in its place.
    #[serde(default)]
    pub payload: Option<Value>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AcceptType {
    pub hash: String,
}

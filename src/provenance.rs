//! Where every word of a document came from, for the provenance preview.
//!
//! The last look at a finished document is a skim, and skimming is how drift
//! gets through. So the preview marks text by origin, worked out from the
//! revision history rather than trusted from anyone's say-so:
//!
//! - unmarked: the person wrote it, or an agent wrote it and the person has
//!   since marked the record reviewed (or approved the document);
//! - light: agent wording the person accepted on a card, at the words that
//!   changed from what was there before;
//! - strong: agent text the person hasn't reviewed;
//!
//! plus a marker on anything that changed since the person last approved the
//! document, with removed words struck through. hoja's own code renders this
//! view, so a template can't hide any of it.

use std::collections::{BTreeSet, HashMap};
use std::sync::LazyLock;

use regex::Regex;
use serde::Serialize;
use serde_json::{Map, Value};
use similar::{Algorithm, DiffOp, capture_diff_slices};

use crate::config::{FieldConfig, FieldType};
use crate::gates::Author;
use crate::model::{Record, Revision};
use crate::store::Resolved;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Origin {
    /// Written by the person.
    Person,
    /// Written by an agent directly, then reviewed by the person.
    Reviewed,
    /// An agent's wording the person accepted on a card.
    Accepted,
    /// Written by an agent directly and not reviewed since.
    Agent,
    /// Separators hoja adds between list entries.
    Layout,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Span {
    pub text: String,
    pub origin: Origin,
    /// Changed since the person last approved the document.
    #[serde(skip_serializing_if = "is_false")]
    pub changed: bool,
    /// Was in the approved document and is gone now.
    #[serde(skip_serializing_if = "is_false")]
    pub removed: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub agent: Option<String>,
    /// The revision that introduced this text.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub version: Option<i64>,
}

fn is_false(b: &bool) -> bool {
    !*b
}

#[derive(Debug, Clone, Serialize)]
pub struct FieldView {
    pub name: String,
    pub title: String,
    /// A text field, shown as prose.
    pub long: bool,
    pub spans: Vec<Span>,
}

#[derive(Debug, Clone, Serialize)]
pub struct ItemView {
    pub id: String,
    #[serde(rename = "type")]
    pub type_name: String,
    pub kind: String,
    pub reviewed: bool,
    /// Selected since the person last approved the document.
    pub added: bool,
    pub label: Vec<Span>,
    pub fields: Vec<FieldView>,
}

#[derive(Debug, Clone, Serialize)]
pub struct GroupView {
    pub heading: Option<Vec<Span>>,
    pub items: Vec<ItemView>,
}

#[derive(Debug, Clone, Serialize)]
pub struct SectionView {
    pub name: String,
    pub title: String,
    pub groups: Vec<GroupView>,
    /// Labels of records the approved document selected here and this one doesn't.
    pub removed: Vec<String>,
}

#[derive(Debug, Clone, Default, Serialize)]
pub struct Summary {
    /// Words an agent wrote that the person hasn't reviewed.
    pub agent_words: usize,
    /// Words of agent wording accepted on cards.
    pub accepted_words: usize,
    pub changed_words: usize,
    pub removed_words: usize,
}

#[derive(Debug, Clone, Serialize)]
pub struct Provenance {
    /// Whether hoja knows what the person last approved, so changes can be marked.
    pub baseline: bool,
    pub title: Vec<Span>,
    pub fields: Vec<FieldView>,
    pub sections: Vec<SectionView>,
    pub summary: Summary,
}

/// What `build` needs beyond the resolved document.
pub struct Inputs<'a> {
    pub resolved: &'a Resolved,
    pub document_history: &'a [Revision],
    /// Revisions of every selected and referenced record, by id.
    pub record_history: &'a HashMap<String, Vec<Revision>>,
    /// The content the person last approved, as stored at approval.
    pub approved: Option<&'a Value>,
    /// Labels for records that ref fields name, by id.
    pub labels: &'a HashMap<String, String>,
}

static WORDS: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"\s+|\S+").expect("valid regex"));
const TITLE_KEY: &str = "@title";

#[derive(Debug, Clone)]
struct Tok {
    text: String,
    author: Author,
    agent: Option<String>,
    version: Option<i64>,
    reviewed: bool,
}

impl Tok {
    fn origin(&self) -> Origin {
        match self.author {
            Author::Person => Origin::Person,
            Author::AgentAccepted => Origin::Accepted,
            Author::Agent if self.reviewed => Origin::Reviewed,
            Author::Agent => Origin::Agent,
        }
    }
}

fn display(value: &Value) -> String {
    match value {
        Value::String(s) => s.clone(),
        Value::Bool(b) => if *b { "yes" } else { "no" }.into(),
        Value::Null => String::new(),
        other => other.to_string(),
    }
}

/// A value as diffable tokens: words and the whitespace between them for
/// strings, one token per entry for lists. The flag says it was a list.
fn tokens(value: Option<&Value>) -> (Vec<String>, bool) {
    match value {
        None | Some(Value::Null) => (Vec::new(), false),
        Some(Value::String(s)) => (
            WORDS.find_iter(s).map(|m| m.as_str().to_string()).collect(),
            false,
        ),
        Some(Value::Array(items)) => (items.iter().map(display).collect(), true),
        Some(other) => (vec![display(other)], false),
    }
}

fn diff<'a>(old: &[&'a str], new: &[&'a str]) -> Vec<DiffOp> {
    capture_diff_slices(Algorithm::Myers, old, new)
}

/// The tokens of `new`, keeping the attribution of every token that survived
/// from `old` and attributing the rest with `make`.
fn merge(old: Vec<Tok>, new: &[String], make: &dyn Fn(&str) -> Tok) -> Vec<Tok> {
    let old_texts: Vec<&str> = old.iter().map(|t| t.text.as_str()).collect();
    let new_texts: Vec<&str> = new.iter().map(String::as_str).collect();
    let mut out = Vec::with_capacity(new.len());
    for op in diff(&old_texts, &new_texts) {
        match op {
            DiffOp::Equal { old_index, len, .. } => {
                out.extend(old[old_index..old_index + len].iter().cloned());
            }
            DiffOp::Delete { .. } => {}
            DiffOp::Insert {
                new_index, new_len, ..
            }
            | DiffOp::Replace {
                new_index, new_len, ..
            } => {
                out.extend(new[new_index..new_index + new_len].iter().map(|t| make(t)));
            }
        }
    }
    out
}

/// Per field (and the title, for documents): its current tokens, each
/// attributed to the revision that introduced it.
type State = HashMap<String, (Vec<Tok>, bool)>;

fn apply(state: &mut State, values: &Map<String, Value>, make: &dyn Fn(&str) -> Tok) {
    let keys: BTreeSet<String> = state.keys().chain(values.keys()).cloned().collect();
    for key in keys {
        let (texts, list) = tokens(values.get(&key));
        let old = state.remove(&key).map(|(t, _)| t).unwrap_or_default();
        let merged = merge(old, &texts, make);
        if !merged.is_empty() {
            state.insert(key, (merged, list));
        }
    }
}

fn snapshot_values(snapshot: &Value, with_title: bool) -> Map<String, Value> {
    let mut values = snapshot
        .get("fields")
        .and_then(Value::as_object)
        .cloned()
        .unwrap_or_default();
    if with_title && let Some(title) = snapshot.get("title") {
        values.insert(TITLE_KEY.into(), title.clone());
    }
    values
}

/// Replays a target's revisions oldest first, then reconciles with its
/// current values in case history is incomplete (anything unexplained is
/// treated as unreviewed agent text, the conservative reading).
fn attribute(revisions: &[Revision], current: &Map<String, Value>, with_title: bool) -> State {
    let mut ordered: Vec<&Revision> = revisions.iter().collect();
    ordered.sort_by_key(|r| r.version);
    let mut state = State::new();
    for revision in ordered {
        match revision.change.as_str() {
            "reviewed" | "approve" => set_reviewed(&mut state, true),
            "unreviewed" => set_reviewed(&mut state, false),
            "delete" => {}
            _ => {
                let make = |text: &str| Tok {
                    text: text.to_string(),
                    author: revision.author,
                    agent: revision.agent.clone(),
                    version: Some(revision.version),
                    reviewed: false,
                };
                apply(
                    &mut state,
                    &snapshot_values(&revision.snapshot, with_title),
                    &make,
                );
            }
        }
    }
    let unknown = |text: &str| Tok {
        text: text.to_string(),
        author: Author::Agent,
        agent: None,
        version: None,
        reviewed: false,
    };
    apply(&mut state, current, &unknown);
    state
}

fn set_reviewed(state: &mut State, reviewed: bool) {
    for (toks, _) in state.values_mut() {
        for tok in toks.iter_mut().filter(|t| t.author == Author::Agent) {
            tok.reviewed = reviewed;
        }
    }
}

fn blank(text: &str) -> bool {
    text.trim().is_empty()
}

struct Spanner<'a> {
    labels: &'a HashMap<String, String>,
    summary: Summary,
}

impl Spanner<'_> {
    /// The spans for one value: its attributed tokens, compared with the
    /// approved value when there is one.
    fn spans(
        &mut self,
        state: Option<&(Vec<Tok>, bool)>,
        baseline: Option<Option<&Value>>,
        field: Option<&FieldConfig>,
    ) -> Option<Vec<Span>> {
        let (toks, list) = state
            .map(|(t, l)| (t.as_slice(), *l))
            .unwrap_or((&[], false));
        let (old, old_list) = baseline.map(tokens).unwrap_or_default();
        if toks.is_empty() && old.is_empty() {
            return None;
        }
        let list = list || old_list;
        let is_ref = field.is_some_and(|f| f.ty == FieldType::Ref);
        let shown = |text: &str| -> String {
            if is_ref {
                self.labels
                    .get(text)
                    .cloned()
                    .unwrap_or_else(|| text.to_string())
            } else {
                text.to_string()
            }
        };
        let span = |tok: &Tok, changed: bool| Span {
            text: shown(&tok.text),
            origin: tok.origin(),
            changed,
            removed: false,
            agent: tok.agent.clone(),
            version: tok.version,
        };
        let gone = |text: &str| Span {
            text: shown(text),
            origin: Origin::Person,
            changed: false,
            removed: true,
            agent: None,
            version: None,
        };
        let mut raw: Vec<Span> = Vec::new();
        match baseline {
            None => raw.extend(toks.iter().map(|t| span(t, false))),
            Some(_) => {
                let old_texts: Vec<&str> = old.iter().map(String::as_str).collect();
                let new_texts: Vec<&str> = toks.iter().map(|t| t.text.as_str()).collect();
                for op in diff(&old_texts, &new_texts) {
                    match op {
                        DiffOp::Equal { new_index, len, .. } => {
                            raw.extend(
                                toks[new_index..new_index + len]
                                    .iter()
                                    .map(|t| span(t, false)),
                            );
                        }
                        DiffOp::Delete {
                            old_index, old_len, ..
                        } => {
                            raw.extend(old[old_index..old_index + old_len].iter().map(|t| gone(t)))
                        }
                        DiffOp::Insert {
                            new_index, new_len, ..
                        } => raw.extend(
                            toks[new_index..new_index + new_len]
                                .iter()
                                .map(|t| span(t, true)),
                        ),
                        DiffOp::Replace {
                            old_index,
                            old_len,
                            new_index,
                            new_len,
                        } => {
                            raw.extend(old[old_index..old_index + old_len].iter().map(|t| gone(t)));
                            raw.extend(
                                toks[new_index..new_index + new_len]
                                    .iter()
                                    .map(|t| span(t, true)),
                            );
                        }
                    }
                }
            }
        }
        for s in raw.iter().filter(|s| !blank(&s.text)) {
            let words = if list {
                s.text.split_whitespace().count().max(1)
            } else {
                1
            };
            if s.removed {
                self.summary.removed_words += words;
                continue;
            }
            if s.changed {
                self.summary.changed_words += words;
            }
            match s.origin {
                Origin::Agent => self.summary.agent_words += words,
                Origin::Accepted => self.summary.accepted_words += words,
                _ => {}
            }
        }
        let spans = if list { separate(raw) } else { smooth(raw) };
        Some(join(spans))
    }
}

/// List entries joined by separators.
fn separate(raw: Vec<Span>) -> Vec<Span> {
    let mut out = Vec::new();
    for (i, span) in raw.into_iter().enumerate() {
        if i > 0 {
            out.push(Span {
                text: ", ".into(),
                origin: Origin::Layout,
                changed: false,
                removed: false,
                agent: None,
                version: None,
            });
        }
        out.push(span);
    }
    out
}

/// Whitespace takes the marking of the words around it when they agree, so a
/// highlighted phrase reads as one run; otherwise it is unmarked.
fn smooth(mut raw: Vec<Span>) -> Vec<Span> {
    let word = |s: &Span| !blank(&s.text) && !s.removed;
    for i in 0..raw.len() {
        if !blank(&raw[i].text) || raw[i].removed {
            continue;
        }
        let before = raw[..i].iter().rev().find(|s| word(s)).cloned();
        let after = raw[i + 1..].iter().find(|s| word(s)).cloned();
        let target = &mut raw[i];
        match (before, after) {
            (Some(b), Some(a))
                if b.origin == a.origin
                    && b.changed == a.changed
                    && b.agent == a.agent
                    && b.version == a.version =>
            {
                target.origin = b.origin;
                target.changed = b.changed;
                target.agent = b.agent;
                target.version = b.version;
            }
            _ => {
                target.origin = Origin::Person;
                target.changed = false;
                target.agent = None;
                target.version = None;
            }
        }
    }
    raw
}

/// Merges neighbouring spans with the same marking.
fn join(spans: Vec<Span>) -> Vec<Span> {
    let mut out: Vec<Span> = Vec::new();
    for span in spans {
        if let Some(last) = out.last_mut()
            && last.origin == span.origin
            && last.changed == span.changed
            && last.removed == span.removed
            && last.agent == span.agent
            && last.version == span.version
        {
            last.text.push_str(&span.text);
        } else {
            out.push(span);
        }
    }
    out
}

pub fn field_title(name: &str, field: Option<&FieldConfig>) -> String {
    if let Some(title) = field.and_then(|f| f.title.clone()) {
        return title;
    }
    let spaced = name.replace('_', " ");
    let mut chars = spaced.chars();
    chars
        .next()
        .map(|c| c.to_uppercase().chain(chars).collect())
        .unwrap_or_default()
}

/// Approved records by id: every selected and referenced record's fields.
fn approved_records(approved: &Value) -> HashMap<String, (String, Map<String, Value>)> {
    let mut out = HashMap::new();
    for section in approved["sections"].as_array().into_iter().flatten() {
        let name = section["name"].as_str().unwrap_or_default();
        for item in section["items"].as_array().into_iter().flatten() {
            if let (Some(id), Some(fields)) = (item["id"].as_str(), item["fields"].as_object()) {
                out.insert(id.to_string(), (name.to_string(), fields.clone()));
            }
        }
    }
    for item in approved["refs"].as_array().into_iter().flatten() {
        if let (Some(id), Some(fields)) = (item["id"].as_str(), item["fields"].as_object()) {
            out.entry(id.to_string())
                .or_insert_with(|| (String::new(), fields.clone()));
        }
    }
    out
}

pub fn build(inputs: Inputs<'_>) -> Provenance {
    let resolved = inputs.resolved;
    let config = &resolved.config;
    let mut spanner = Spanner {
        labels: inputs.labels,
        summary: Summary::default(),
    };
    let approved = inputs.approved;
    let approved_items = approved.map(approved_records).unwrap_or_default();

    // The document's own title and fields.
    let mut current = resolved.document.fields.clone();
    current.insert(
        TITLE_KEY.into(),
        Value::String(resolved.document.title.clone()),
    );
    let state = attribute(inputs.document_history, &current, true);
    let mut baseline_doc = approved
        .and_then(|a| a["fields"].as_object().cloned())
        .unwrap_or_default();
    if let Some(a) = approved {
        baseline_doc.insert(TITLE_KEY.into(), a["title"].clone());
    }
    let doc_baseline = |key: &str| approved.map(|_| baseline_doc.get(key));
    let title = spanner
        .spans(state.get(TITLE_KEY), doc_baseline(TITLE_KEY), None)
        .unwrap_or_default();
    let fields = config
        .document
        .fields
        .iter()
        .filter_map(|(name, field)| {
            let spans = spanner.spans(state.get(name), doc_baseline(name), Some(field))?;
            Some(FieldView {
                name: name.clone(),
                title: field_title(name, Some(field)),
                long: field.ty == FieldType::Text,
                spans,
            })
        })
        .collect();

    let item_view = |record: &Record, spanner: &mut Spanner<'_>| -> ItemView {
        let history = inputs
            .record_history
            .get(&record.id)
            .map(Vec::as_slice)
            .unwrap_or(&[]);
        let state = attribute(history, &record.fields, false);
        let approved_fields = approved_items.get(&record.id).map(|(_, f)| f);
        let added = approved.is_some() && approved_fields.is_none();
        let empty = Map::new();
        let baseline = |name: &str| approved.map(|_| approved_fields.unwrap_or(&empty).get(name));
        let kind = resolved.kind_config(record);
        let label_field = kind.and_then(|k| k.label_field()).map(String::from);
        let label = label_field
            .as_deref()
            .and_then(|name| {
                spanner.spans(
                    state.get(name),
                    baseline(name),
                    kind.and_then(|k| k.fields.get(name)),
                )
            })
            .unwrap_or_else(|| {
                vec![Span {
                    text: format!("{} {}", record.kind, &record.id[..record.id.len().min(8)]),
                    origin: Origin::Layout,
                    changed: added,
                    removed: false,
                    agent: None,
                    version: None,
                }]
            });
        let names: Vec<String> = match kind {
            Some(kind) => kind.fields.keys().cloned().collect(),
            None => record.fields.keys().cloned().collect(),
        };
        let fields = names
            .into_iter()
            .filter(|name| Some(name) != label_field.as_ref())
            .filter_map(|name| {
                let field = kind.and_then(|k| k.fields.get(&name));
                let spans = spanner.spans(state.get(&name), baseline(&name), field)?;
                Some(FieldView {
                    title: field_title(&name, field),
                    long: field.is_some_and(|f| f.ty == FieldType::Text),
                    name,
                    spans,
                })
            })
            .collect();
        ItemView {
            id: record.id.clone(),
            type_name: record.type_name.clone(),
            kind: record.kind.clone(),
            reviewed: record.reviewed,
            added,
            label,
            fields,
        }
    };

    let mut sections = Vec::new();
    for (section_config, section) in config.document.sections.iter().zip(&resolved.sections) {
        let mut groups: Vec<GroupView> = Vec::new();
        let mut group_keys: Vec<Option<String>> = Vec::new();
        for record in &section.records {
            let target = section_config
                .group_by
                .as_deref()
                .and_then(|g| record.fields.get(g))
                .and_then(Value::as_str)
                .and_then(|id| resolved.refs.get(id));
            let key = target.map(|t| t.id.clone());
            let index = match group_keys.iter().position(|k| *k == key) {
                Some(index) => index,
                None => {
                    let heading = target.map(|t| item_view(t, &mut spanner).label);
                    group_keys.push(key);
                    groups.push(GroupView {
                        heading,
                        items: Vec::new(),
                    });
                    groups.len() - 1
                }
            };
            let view = item_view(record, &mut spanner);
            groups[index].items.push(view);
        }
        let current: BTreeSet<&str> = section.records.iter().map(|r| r.id.as_str()).collect();
        let label_of = |fields: &Map<String, Value>| -> String {
            let kind = inputs
                .resolved
                .configs
                .get(config.section_type(section_config, &resolved.document.type_name))
                .and_then(|c| c.kind(&section_config.kind));
            kind.and_then(|k| k.label_field())
                .and_then(|f| fields.get(f))
                .map(display)
                .unwrap_or_else(|| "a record".into())
        };
        let removed = approved_items
            .iter()
            .filter(|(id, (name, _))| *name == section.name && !current.contains(id.as_str()))
            .map(|(_, (_, fields))| label_of(fields))
            .collect();
        sections.push(SectionView {
            name: section.name.clone(),
            title: section_config
                .title
                .clone()
                .unwrap_or_else(|| section_config.name.clone()),
            groups,
            removed,
        });
    }

    Provenance {
        baseline: approved.is_some(),
        title,
        fields,
        sections,
        summary: spanner.summary,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn revision(version: i64, author: Author, change: &str, fields: Value) -> Revision {
        Revision {
            version,
            author,
            agent: (author != Author::Person).then(|| "claude".to_string()),
            approval_id: None,
            edited: false,
            change: change.into(),
            snapshot: json!({"fields": fields}),
            created_at: String::new(),
        }
    }

    fn marked(spans: &[Span]) -> Vec<(String, Origin)> {
        spans
            .iter()
            .filter(|s| matches!(s.origin, Origin::Accepted | Origin::Agent))
            .map(|s| (s.text.clone(), s.origin))
            .collect()
    }

    #[test]
    fn accepted_rewording_marks_only_the_changed_words() {
        let history = vec![
            revision(
                1,
                Author::Person,
                "create",
                json!({"text": "Led the migration to Postgres"}),
            ),
            revision(
                2,
                Author::AgentAccepted,
                "update",
                json!({"text": "Led the zero-downtime migration to Postgres 16"}),
            ),
        ];
        let current = history[1].snapshot["fields"].as_object().unwrap().clone();
        let state = attribute(&history, &current, false);
        let labels = HashMap::new();
        let mut spanner = Spanner {
            labels: &labels,
            summary: Summary::default(),
        };
        let spans = spanner.spans(state.get("text"), None, None).unwrap();
        assert_eq!(
            marked(&spans),
            vec![
                ("zero-downtime".into(), Origin::Accepted),
                ("16".into(), Origin::Accepted)
            ]
        );
        assert_eq!(spanner.summary.accepted_words, 2);
        let text: String = spans.iter().map(|s| s.text.as_str()).collect();
        assert_eq!(text, "Led the zero-downtime migration to Postgres 16");
    }

    #[test]
    fn agent_text_is_strong_until_reviewed() {
        let mut history = vec![revision(
            1,
            Author::Agent,
            "create",
            json!({"text": "Shipped sync"}),
        )];
        let current = history[0].snapshot["fields"].as_object().unwrap().clone();
        let labels = HashMap::new();
        let mut spanner = Spanner {
            labels: &labels,
            summary: Summary::default(),
        };
        let state = attribute(&history, &current, false);
        assert_eq!(
            marked(&spanner.spans(state.get("text"), None, None).unwrap()),
            vec![("Shipped sync".into(), Origin::Agent)]
        );
        history.push(Revision {
            change: "reviewed".into(),
            ..revision(
                2,
                Author::Person,
                "reviewed",
                json!({"text": "Shipped sync"}),
            )
        });
        let state = attribute(&history, &current, false);
        let spans = spanner.spans(state.get("text"), None, None).unwrap();
        assert!(marked(&spans).is_empty(), "{spans:?}");
        assert_eq!(spans[0].origin, Origin::Reviewed);
    }

    #[test]
    fn changes_since_approval_are_marked_and_removals_struck() {
        let history = vec![revision(
            1,
            Author::Person,
            "create",
            json!({"text": "Ship the beta in March"}),
        )];
        let current = Map::from_iter([("text".to_string(), json!("Ship the beta in May"))]);
        let state = attribute(&history, &current, false);
        let labels = HashMap::new();
        let mut spanner = Spanner {
            labels: &labels,
            summary: Summary::default(),
        };
        let approved = json!("Ship the beta in March");
        let spans = spanner
            .spans(state.get("text"), Some(Some(&approved)), None)
            .unwrap();
        let removed: Vec<&str> = spans
            .iter()
            .filter(|s| s.removed)
            .map(|s| s.text.as_str())
            .collect();
        let changed: Vec<&str> = spans
            .iter()
            .filter(|s| s.changed)
            .map(|s| s.text.as_str())
            .collect();
        assert_eq!(removed, vec!["March"]);
        assert_eq!(changed, vec!["May"]);
        // The unexplained edit (no revision wrote "May") counts as unreviewed agent text.
        assert_eq!(
            spans.iter().find(|s| s.changed).unwrap().origin,
            Origin::Agent
        );
        assert_eq!(
            (spanner.summary.changed_words, spanner.summary.removed_words),
            (1, 1)
        );
    }

    #[test]
    fn list_entries_keep_their_own_origin() {
        let history = vec![
            revision(1, Author::Person, "create", json!({"refs": ["!1"]})),
            revision(2, Author::Agent, "update", json!({"refs": ["!1", "APP-2"]})),
        ];
        let current = history[1].snapshot["fields"].as_object().unwrap().clone();
        let state = attribute(&history, &current, false);
        let labels = HashMap::new();
        let mut spanner = Spanner {
            labels: &labels,
            summary: Summary::default(),
        };
        let spans = spanner.spans(state.get("refs"), None, None).unwrap();
        let text: String = spans.iter().map(|s| s.text.as_str()).collect();
        assert_eq!(text, "!1, APP-2");
        assert_eq!(marked(&spans), vec![("APP-2".into(), Origin::Agent)]);
    }
}

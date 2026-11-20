//! Rendering: the data a template sees, Typst for PDF, and the built-in
//! Markdown/HTML export for report types.

use std::collections::{BTreeMap, HashMap};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use serde_json::{Map, Value, json};

use crate::config::{FieldConfig, FieldType, Format, TypeConfig, placeholders};
use crate::markdown::{self, escape};
use crate::model::{Document, Record};
use crate::store::{Resolved, ResolvedSection};
use crate::templates::TemplateFile;

const TYPST_TIMEOUT: Duration = Duration::from_secs(60);

fn text_fields(fields: &indexmap::IndexMap<String, FieldConfig>) -> impl Iterator<Item = &String> {
    fields
        .iter()
        .filter(|(_, f)| f.ty == FieldType::Text)
        .map(|(name, _)| name)
}

/// A record as a template sees it: its fields at the top level, plus `id`,
/// `kind`, `reviewed`, and `_md` holding each text field's parsed Markdown.
/// Field names start with a letter, so `_md` can't collide with one.
fn item(resolved: &Resolved, record: &Record) -> Value {
    let mut map = Map::new();
    map.insert("id".into(), Value::String(record.id.clone()));
    map.insert("kind".into(), Value::String(record.kind.clone()));
    map.insert("reviewed".into(), Value::Bool(record.reviewed));
    for (k, v) in &record.fields {
        map.insert(k.clone(), v.clone());
    }
    if let Some(kind) = resolved.kind_config(record) {
        map.insert(
            "_md".into(),
            Value::Object(markdown::trees(&record.fields, text_fields(&kind.fields))),
        );
    }
    Value::Object(map)
}

/// Selected records grouped under their `group_by` record, in first-seen order.
fn groups<'a>(resolved: &'a Resolved, name: &str) -> Vec<(Option<&'a Record>, Vec<&'a Record>)> {
    let Some(section) = resolved.sections.iter().find(|s| s.name == name) else {
        return Vec::new();
    };
    let group_by = resolved
        .config
        .section(name)
        .and_then(|s| s.group_by.as_deref());
    let Some(group_by) = group_by else {
        return vec![(None, section.records.iter().collect())];
    };
    let mut out: Vec<(Option<&Record>, Vec<&Record>)> = Vec::new();
    for record in &section.records {
        let target = record
            .fields
            .get(group_by)
            .and_then(Value::as_str)
            .and_then(|id| resolved.refs.get(id));
        let key = target.map(|t| t.id.as_str());
        match out
            .iter_mut()
            .find(|(g, _)| g.map(|g| g.id.as_str()) == key)
        {
            Some((_, items)) => items.push(record),
            None => out.push((target, vec![record])),
        }
    }
    out
}

/// Placeholder content for a field, shaped like a real value.
fn placeholder(name: &str, field: &FieldConfig, index: usize) -> Option<Value> {
    let words = name.replace('_', " ");
    Some(match field.ty {
        FieldType::String => Value::String(format!("Sample {words} {}", index + 1)),
        FieldType::Text => Value::String(format!(
            "Sample {words}, with **emphasis** and a list:\n\n- a first point\n- a second point"
        )),
        FieldType::Tags => json!(["sample"]),
        FieldType::List => json!([format!("first {words}"), format!("second {words}")]),
        FieldType::Enum => Value::String(
            field
                .options
                .get(index % field.options.len().max(1))?
                .clone(),
        ),
        FieldType::Date => Value::String(format!("2026-{:02}-15", (index % 12) + 1)),
        FieldType::Bool => Value::Bool(true),
        FieldType::Ref => return None,
    })
}

fn placeholders_for(
    fields: &indexmap::IndexMap<String, FieldConfig>,
    index: usize,
) -> Map<String, Value> {
    fields
        .iter()
        .filter_map(|(name, field)| placeholder(name, field, index).map(|v| (name.clone(), v)))
        .collect()
}

/// A document built from `samples/<type>.json` (`{"title", "fields",
/// "sections": {"<name>": [{"fields", "reviewed"?}]}}`), or from placeholders
/// when there is no sample, for previewing a template before accepting it.
pub fn sample(
    type_name: &str,
    config: Arc<TypeConfig>,
    configs: HashMap<String, Arc<TypeConfig>>,
    sample: Option<&Value>,
) -> Resolved {
    let object = |v: Option<&Value>| v.and_then(Value::as_object).cloned();
    let fields = object(sample.and_then(|s| s.get("fields")))
        .unwrap_or_else(|| placeholders_for(&config.document.fields, 0));
    let mut sections = Vec::new();
    for (si, section) in config.document.sections.iter().enumerate() {
        let source = section.from_type.as_deref().unwrap_or(type_name);
        let kind = configs.get(source).and_then(|c| c.kind(&section.kind));
        let given = sample
            .and_then(|s| s.get("sections"))
            .and_then(|s| s.get(&section.name))
            .and_then(Value::as_array);
        let items: Vec<(Map<String, Value>, bool)> = match given {
            Some(items) => items
                .iter()
                .map(|i| {
                    (
                        object(i.get("fields")).unwrap_or_default(),
                        i["reviewed"].as_bool().unwrap_or(true),
                    )
                })
                .collect(),
            None => match kind {
                Some(kind) => (0..2)
                    .map(|i| {
                        let mut fields = placeholders_for(&kind.fields, si + i);
                        for (field, value) in &section.filter {
                            fields.insert(field.clone(), Value::String(value.clone()));
                        }
                        (fields, true)
                    })
                    .collect(),
                None => Vec::new(),
            },
        };
        let records = items
            .into_iter()
            .enumerate()
            .map(|(i, (fields, reviewed))| Record {
                id: format!("sample-{}-{}", section.name, i + 1),
                type_name: source.to_string(),
                kind: section.kind.clone(),
                fields,
                reviewed,
                reviewed_at: None,
                version: 1,
                created_at: String::new(),
                updated_at: String::new(),
                deleted: false,
                created_by: None,
                updated_by: None,
            })
            .collect();
        sections.push(ResolvedSection {
            name: section.name.clone(),
            records,
        });
    }
    let title = sample
        .and_then(|s| s.get("title"))
        .and_then(Value::as_str)
        .map(String::from)
        .unwrap_or_else(|| {
            config
                .title
                .clone()
                .unwrap_or_else(|| type_name.to_string())
        });
    let document = Document {
        id: "sample".into(),
        type_name: type_name.into(),
        title,
        fields,
        sections: Vec::new(),
        approved: false,
        approved_at: None,
        version: 1,
        created_at: String::new(),
        updated_at: String::new(),
        created_by: None,
        updated_by: None,
    };
    Resolved {
        config,
        configs,
        document,
        sections,
        refs: HashMap::new(),
        missing: Vec::new(),
        template: Value::Null,
    }
}

/// What a template reads as `json("data.json")`.
pub fn data(resolved: &Resolved, owner: Option<&str>, date: &str) -> Value {
    let config = &resolved.config;
    let mut sections = Map::new();
    for section in &config.document.sections {
        let records = resolved
            .sections
            .iter()
            .find(|s| s.name == section.name)
            .map(|s| {
                s.records
                    .iter()
                    .map(|r| item(resolved, r))
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default();
        let grouped: Vec<Value> = groups(resolved, &section.name)
            .into_iter()
            .map(|(target, items)| {
                json!({
                    "ref": target.map(|r| item(resolved, r)),
                    "items": items.into_iter().map(|r| item(resolved, r)).collect::<Vec<_>>(),
                })
            })
            .collect();
        sections.insert(
            section.name.clone(),
            json!({
                "title": section.title.clone().unwrap_or_else(|| section.name.clone()),
                "items": records,
                "groups": grouped,
            }),
        );
    }
    let document = &resolved.document;
    json!({
        "owner": owner,
        "type": document.type_name,
        "type_title": config.title,
        "date": date,
        "document": {
            "id": document.id,
            "title": document.title,
            "fields": document.fields,
            "_md": markdown::trees(&document.fields, text_fields(&config.document.fields)),
            "approved": document.approved,
            "approved_at": document.approved_at,
        },
        "sections": sections,
        "section_order": config.document.sections.iter().map(|s| s.name.clone()).collect::<Vec<_>>(),
    })
}

fn sanitize(segment: &str) -> String {
    let mut out = String::new();
    for word in segment.split_whitespace() {
        if !out.is_empty() {
            out.push('_');
        }
        out.extend(
            word.chars()
                .filter(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '_')),
        );
    }
    out.trim_matches('.').to_string()
}

/// The export file name from the type's pattern. Unapproved documents say so.
pub fn filename(resolved: &Resolved, owner: Option<&str>, date: &str, format: Format) -> String {
    let pattern = &resolved.config.filename;
    let mut name = pattern.clone();
    for placeholder in placeholders(pattern) {
        let value = match placeholder {
            "owner" => owner.unwrap_or("hoja").to_string(),
            "type" => resolved.document.type_name.clone(),
            "title" => resolved.document.title.clone(),
            "date" => date.to_string(),
            other => other
                .strip_prefix("doc.")
                .and_then(|f| resolved.document.fields.get(f))
                .map(display)
                .unwrap_or_default(),
        };
        name = name.replace(&format!("{{{placeholder}}}"), &sanitize(&value));
    }
    let mut name = sanitize(&name.replace(['/', '\\'], "_"))
        .trim_matches(['_', '-'])
        .to_string();
    if name.is_empty() {
        name = "document".into();
    }
    if !resolved.document.approved {
        name.push_str("-DRAFT");
    }
    format!("{name}.{}", format.extension())
}

fn display(value: &Value) -> String {
    match value {
        Value::String(s) => s.clone(),
        Value::Array(items) => items.iter().map(display).collect::<Vec<_>>().join(", "),
        Value::Bool(b) => if *b { "yes" } else { "no" }.into(),
        Value::Null => String::new(),
        other => other.to_string(),
    }
}

fn label(resolved: &Resolved, record: &Record) -> String {
    resolved
        .kind_config(record)
        .and_then(|k| k.label_field())
        .and_then(|f| record.fields.get(f))
        .map(display)
        .unwrap_or_default()
}

fn item_line(resolved: &Resolved, pattern: Option<&str>, record: &Record) -> String {
    let Some(pattern) = pattern else {
        return label(resolved, record);
    };
    let mut line = pattern.to_string();
    for placeholder in placeholders(pattern) {
        let value = if placeholder == "id" {
            record.id.clone()
        } else {
            record
                .fields
                .get(placeholder)
                .map(display)
                .unwrap_or_default()
        };
        line = line.replace(&format!("{{{placeholder}}}"), &value);
    }
    // An empty optional field leaves its punctuation behind; drop it.
    for empty in [" ()", "()", " []", "[]"] {
        line = line.replace(empty, "");
    }
    line.trim().to_string()
}

/// A section's groups: an optional heading over its item lines.
type Groups = Vec<(Option<String>, Vec<String>)>;

struct Outline {
    title: String,
    fields: Vec<(String, String, bool)>,
    sections: Vec<(String, Groups)>,
}

fn outline(resolved: &Resolved) -> Outline {
    let config = &resolved.config;
    let fields = config
        .document
        .fields
        .iter()
        .filter_map(|(name, field)| {
            let value = resolved.document.fields.get(name).map(display)?;
            let title = field.title.clone().unwrap_or_else(|| {
                let spaced = name.replace('_', " ");
                let mut chars = spaced.chars();
                chars
                    .next()
                    .map(|c| c.to_uppercase().chain(chars).collect())
                    .unwrap_or_default()
            });
            Some((title, value, field.ty == FieldType::Text))
        })
        .collect();
    let sections = config
        .document
        .sections
        .iter()
        .map(|section| {
            let title = section
                .title
                .clone()
                .unwrap_or_else(|| section.name.clone());
            let groups = groups(resolved, &section.name)
                .into_iter()
                .map(|(target, items)| {
                    let heading = target.map(|t| label(resolved, t));
                    let lines = items
                        .into_iter()
                        .map(|r| item_line(resolved, section.item.as_deref(), r))
                        .collect();
                    (heading, lines)
                })
                .collect();
            (title, groups)
        })
        .collect();
    Outline {
        title: resolved.document.title.clone(),
        fields,
        sections,
    }
}

pub fn markdown(resolved: &Resolved) -> String {
    let outline = outline(resolved);
    let mut out = format!("# {}\n", outline.title);
    for (title, value, long) in &outline.fields {
        if *long {
            out.push_str(&format!("\n## {title}\n\n{}\n", value.trim_end()));
        } else {
            out.push_str(&format!("\n**{title}:** {value}\n"));
        }
    }
    for (title, groups) in &outline.sections {
        if groups.iter().all(|(_, lines)| lines.is_empty()) {
            continue;
        }
        out.push_str(&format!("\n## {title}\n"));
        for (heading, lines) in groups {
            if let Some(heading) = heading {
                out.push_str(&format!("\n### {heading}\n"));
            }
            out.push('\n');
            for line in lines {
                out.push_str(&format!("- {line}\n"));
            }
        }
    }
    out
}

pub fn html(resolved: &Resolved) -> String {
    let outline = outline(resolved);
    let mut body = format!("<h1>{}</h1>\n", escape(&outline.title));
    for (title, value, long) in &outline.fields {
        if *long {
            body.push_str(&format!("<h2>{}</h2>\n", escape(title)));
            body.push_str(&markdown::html(value, 2));
        } else {
            body.push_str(&format!(
                "<p><strong>{}:</strong> {}</p>\n",
                escape(title),
                escape(value)
            ));
        }
    }
    for (title, groups) in &outline.sections {
        if groups.iter().all(|(_, lines)| lines.is_empty()) {
            continue;
        }
        body.push_str(&format!("<h2>{}</h2>\n", escape(title)));
        for (heading, lines) in groups {
            if let Some(heading) = heading {
                body.push_str(&format!("<h3>{}</h3>\n", escape(heading)));
            }
            body.push_str("<ul>\n");
            for line in lines {
                body.push_str(&format!("<li>{}</li>\n", escape(line)));
            }
            body.push_str("</ul>\n");
        }
    }
    format!(
        "<!doctype html>\n<html lang=\"en\">\n<head>\n<meta charset=\"utf-8\">\n<meta name=\"viewport\" content=\"width=device-width, initial-scale=1\">\n<title>{}</title>\n<style>body{{font:16px/1.5 system-ui,sans-serif;max-width:46rem;margin:2rem auto;padding:0 1rem;color:#1d1d1f}}h1{{font-size:1.6rem}}h2{{font-size:1.2rem;margin-top:2rem}}h3{{font-size:1rem}}</style>\n</head>\n<body>\n{body}</body>\n</html>\n",
        escape(&outline.title)
    )
}

#[derive(Debug)]
pub enum RenderError {
    NoTemplate,
    /// Typst ran and refused; its diagnostics, verbatim.
    Compile(String),
    Io(String),
}

impl std::fmt::Display for RenderError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NoTemplate => f.write_str("this type has no template, so it has no PDF"),
            Self::Compile(message) => write!(f, "Typst could not compile the template:\n{message}"),
            Self::Io(message) => f.write_str(message),
        }
    }
}

pub struct Typst {
    pub binary: PathBuf,
    /// Extra fonts, used when the directory exists.
    pub fonts: PathBuf,
}

impl Typst {
    /// Compiles `entry` against `data` in a scratch root holding only the
    /// given (accepted) template files and `data.json`, so a template can't
    /// reach the workspace, its database, or a template file nobody accepted.
    pub async fn pdf(
        &self,
        files: &BTreeMap<String, TemplateFile>,
        entry: &str,
        data: &Value,
    ) -> Result<Vec<u8>, RenderError> {
        let io = |e: std::io::Error| RenderError::Io(e.to_string());
        let scratch = tempfile::tempdir().map_err(io)?;
        let root = scratch.path().join("root");
        for (path, file) in files {
            let target = root.join(path);
            if let Some(parent) = target.parent() {
                std::fs::create_dir_all(parent).map_err(io)?;
            }
            std::fs::write(&target, file.body.as_slice()).map_err(io)?;
        }
        std::fs::create_dir_all(&root).map_err(io)?;
        std::fs::write(root.join("data.json"), data.to_string()).map_err(io)?;
        let output = scratch.path().join("out.pdf");
        let mut command = tokio::process::Command::new(&self.binary);
        command
            .arg("compile")
            .arg("--root")
            .arg(&root)
            .arg(root.join(entry))
            .arg(&output)
            .current_dir(scratch.path())
            .kill_on_drop(true);
        if self.fonts.is_dir() {
            command.arg("--font-path").arg(&self.fonts);
        }
        let run = tokio::time::timeout(TYPST_TIMEOUT, command.output())
            .await
            .map_err(|_| RenderError::Compile("timed out after 60 seconds".into()))?
            .map_err(|e| {
                let hint = if e.kind() == std::io::ErrorKind::NotFound {
                    ". PDFs need the Typst compiler: install it, or name it with \
                     --typst (hoja serve) or `typst` in <data>/plugin.toml (as a Tracon plugin)"
                } else {
                    ""
                };
                RenderError::Io(format!("running {}: {e}{hint}", self.binary.display()))
            })?;
        if !run.status.success() {
            let stderr = String::from_utf8_lossy(&run.stderr);
            // Typst names files by absolute path or relative to its working
            // directory (the scratch dir); show both as `templates/...`.
            let root_text = root.display().to_string();
            let message = stderr
                .replace(&root_text, "templates")
                .replace("─ root/", "─ templates/");
            return Err(RenderError::Compile(message.trim().to_string()));
        }
        std::fs::read(&output).map_err(io)
    }
}

#[cfg(test)]
mod tests {
    use super::sanitize;

    #[test]
    fn sanitize_keeps_safe_characters() {
        assert_eq!(sanitize("Jane Doe"), "Jane_Doe");
        assert_eq!(sanitize("../etc/passwd"), "etcpasswd");
        assert_eq!(sanitize("Oct 2026: report!"), "Oct_2026_report");
    }
}

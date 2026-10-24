//! Rendering: the data a template sees, Typst for PDF, and the built-in
//! Markdown/HTML export for report types.

use std::path::{Path, PathBuf};
use std::time::Duration;

use serde_json::{Map, Value, json};

use crate::config::{FieldType, Format, TypeConfig, placeholders};
use crate::model::Record;
use crate::store::Resolved;

const TYPST_TIMEOUT: Duration = Duration::from_secs(60);

fn item(record: &Record) -> Value {
    let mut map = Map::new();
    map.insert("id".into(), Value::String(record.id.clone()));
    map.insert("kind".into(), Value::String(record.kind.clone()));
    map.insert("reviewed".into(), Value::Bool(record.reviewed));
    for (k, v) in &record.fields {
        map.insert(k.clone(), v.clone());
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

/// What a template reads as `json("data.json")`.
pub fn data(resolved: &Resolved, owner: Option<&str>, date: &str) -> Value {
    let config = &resolved.config;
    let mut sections = Map::new();
    for section in &config.document.sections {
        let records = resolved
            .sections
            .iter()
            .find(|s| s.name == section.name)
            .map(|s| s.records.iter().map(item).collect::<Vec<_>>())
            .unwrap_or_default();
        let grouped: Vec<Value> = groups(resolved, &section.name)
            .into_iter()
            .map(|(target, items)| {
                json!({
                    "ref": target.map(item),
                    "items": items.into_iter().map(item).collect::<Vec<_>>(),
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

fn label(config: &TypeConfig, record: &Record) -> String {
    config
        .kind(&record.kind)
        .and_then(|k| k.label_field())
        .and_then(|f| record.fields.get(f))
        .map(display)
        .unwrap_or_default()
}

fn item_line(config: &TypeConfig, pattern: Option<&str>, record: &Record) -> String {
    let Some(pattern) = pattern else {
        return label(config, record);
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
                    let heading = target.map(|t| label(config, t));
                    let lines = items
                        .into_iter()
                        .map(|r| item_line(config, section.item.as_deref(), r))
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
            out.push_str(&format!("\n{value}\n"));
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

fn escape(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&#39;")
}

pub fn html(resolved: &Resolved) -> String {
    let outline = outline(resolved);
    let mut body = format!("<h1>{}</h1>\n", escape(&outline.title));
    for (title, value, long) in &outline.fields {
        if *long {
            for paragraph in value.split("\n\n") {
                body.push_str(&format!("<p>{}</p>\n", escape(paragraph.trim())));
            }
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

fn copy_dir(from: &Path, to: &Path) -> std::io::Result<()> {
    std::fs::create_dir_all(to)?;
    for entry in std::fs::read_dir(from)? {
        let entry = entry?;
        let target = to.join(entry.file_name());
        let kind = entry.file_type()?;
        if kind.is_dir() {
            copy_dir(&entry.path(), &target)?;
        } else if kind.is_file() {
            std::fs::copy(entry.path(), target)?;
        }
    }
    Ok(())
}

pub struct Typst {
    pub binary: PathBuf,
    pub templates: PathBuf,
    /// Extra fonts, used when the directory exists.
    pub fonts: PathBuf,
}

impl Typst {
    /// Compiles the type's template against `data` in a scratch root holding
    /// a copy of `templates/` and `data.json`, so templates can't reach the
    /// workspace or its database.
    pub async fn pdf(&self, config: &TypeConfig, data: &Value) -> Result<Vec<u8>, RenderError> {
        let template = config.template.as_deref().ok_or(RenderError::NoTemplate)?;
        let io = |e: std::io::Error| RenderError::Io(e.to_string());
        let root = tempfile::tempdir().map_err(io)?;
        copy_dir(&self.templates, root.path())
            .map_err(|e| RenderError::Io(format!("copying templates/: {e}")))?;
        let entry = root.path().join(template);
        if !entry.is_file() {
            return Err(RenderError::Io(format!(
                "templates/{template} does not exist"
            )));
        }
        std::fs::write(root.path().join("data.json"), data.to_string()).map_err(io)?;
        let output = root.path().join("hoja-out.pdf");
        let mut command = tokio::process::Command::new(&self.binary);
        command
            .arg("compile")
            .arg("--root")
            .arg(root.path())
            .arg(&entry)
            .arg(&output)
            .kill_on_drop(true);
        if self.fonts.is_dir() {
            command.arg("--font-path").arg(&self.fonts);
        }
        let run = tokio::time::timeout(TYPST_TIMEOUT, command.output())
            .await
            .map_err(|_| RenderError::Compile("timed out after 60 seconds".into()))?
            .map_err(|e| RenderError::Io(format!("running {}: {e}", self.binary.display())))?;
        if !run.status.success() {
            let stderr = String::from_utf8_lossy(&run.stderr);
            let root_text = root.path().display().to_string();
            return Err(RenderError::Compile(
                stderr.replace(&root_text, "templates").trim().to_string(),
            ));
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

//! Type-defined checks over a document's selected content.

use std::collections::HashSet;

use regex::Regex;
use serde_json::{Map, Value};

use crate::config::{DOCUMENT_KIND, Level, Lint};
use crate::model::{LintFinding, Record};
use crate::store::Resolved;

fn texts(value: Option<&Value>) -> Vec<&str> {
    match value {
        Some(Value::String(s)) => vec![s.as_str()],
        Some(Value::Array(items)) => items.iter().filter_map(Value::as_str).collect(),
        _ => Vec::new(),
    }
}

fn finding(level: Level, message: String, record: Option<&Record>, field: Option<&str>) -> LintFinding {
    LintFinding {
        level,
        message,
        record_id: record.map(|r| r.id.clone()),
        field: field.map(String::from),
    }
}

/// A short name for a record in messages.
pub fn record_label(resolved: &Resolved, record: &Record) -> String {
    let label = resolved
        .config
        .kind(&record.kind)
        .and_then(|k| k.label_field())
        .and_then(|f| record.fields.get(f))
        .and_then(Value::as_str)
        .unwrap_or(&record.id);
    let mut short: String = label.chars().take(48).collect();
    if short.len() < label.len() {
        short.push('…');
    }
    format!("{} \"{short}\"", record.kind)
}

pub fn lint(resolved: &Resolved) -> Vec<LintFinding> {
    let mut findings = Vec::new();
    for (section, id) in &resolved.missing {
        findings.push(LintFinding {
            level: Level::Error,
            message: format!("section `{section}` selects a deleted record"),
            record_id: Some(id.clone()),
            field: None,
        });
    }
    let mut seen = HashSet::new();
    let records: Vec<&Record> = resolved
        .sections
        .iter()
        .flat_map(|s| s.records.iter())
        .filter(|r| seen.insert(r.id.clone()))
        .collect();
    let document_fields: &Map<String, Value> = &resolved.document.fields;
    let of_kind = |kind: &str| -> Vec<&Record> { records.iter().copied().filter(|r| r.kind == kind).collect() };

    for lint in &resolved.config.lints {
        match lint {
            Lint::Forbid { pattern, kind, fields, level, message } => {
                let Ok(re) = Regex::new(pattern) else { continue };
                let wanted = |name: &str| fields.is_empty() || fields.iter().any(|f| f == name);
                let mut check = |record: Option<&Record>, map: &Map<String, Value>| {
                    for (name, value) in map {
                        if !wanted(name) {
                            continue;
                        }
                        for text in texts(Some(value)) {
                            if let Some(hit) = re.find(text) {
                                let whose = record.map(|r| record_label(resolved, r)).unwrap_or_else(|| "document".into());
                                let text = message.clone().unwrap_or_else(|| format!("contains \"{}\"", hit.as_str()));
                                findings.push(finding(*level, format!("{whose}, {name}: {text}"), record, Some(name)));
                                break;
                            }
                        }
                    }
                };
                match kind.as_deref() {
                    Some(DOCUMENT_KIND) => check(None, document_fields),
                    Some(kind) => {
                        for record in of_kind(kind) {
                            check(Some(record), &record.fields);
                        }
                    }
                    None => {
                        check(None, document_fields);
                        for record in &records {
                            check(Some(record), &record.fields);
                        }
                    }
                }
            }
            Lint::Require { kind, field, level, message } => {
                let text = message.clone().unwrap_or_else(|| format!("{field} is required"));
                if kind == DOCUMENT_KIND {
                    if !document_fields.contains_key(field) {
                        findings.push(finding(*level, format!("document: {text}"), None, Some(field)));
                    }
                } else {
                    for record in of_kind(kind) {
                        if !record.fields.contains_key(field) {
                            let whose = record_label(resolved, record);
                            findings.push(finding(*level, format!("{whose}: {text}"), Some(record), Some(field)));
                        }
                    }
                }
            }
            Lint::Match { kind, field, pattern, min, level, message } => {
                let Ok(re) = Regex::new(pattern) else { continue };
                let mut check = |record: Option<&Record>, value: Option<&Value>| {
                    let values = texts(value);
                    let bad: Vec<&str> = values.iter().copied().filter(|v| !re.is_match(v)).collect();
                    let whose = record.map(|r| record_label(resolved, r)).unwrap_or_else(|| "document".into());
                    if values.len() < *min || !bad.is_empty() {
                        let text = message.clone().unwrap_or_else(|| {
                            if bad.is_empty() {
                                format!("{field} needs at least {min}")
                            } else {
                                format!("{field} has entries that don't match: {}", bad.join(", "))
                            }
                        });
                        findings.push(finding(*level, format!("{whose}: {text}"), record, Some(field)));
                    }
                };
                if kind == DOCUMENT_KIND {
                    check(None, document_fields.get(field));
                } else {
                    for record in of_kind(kind) {
                        check(Some(record), record.fields.get(field));
                    }
                }
            }
            Lint::MaxWords { kind, field, max, level, message } => {
                let mut check = |record: Option<&Record>, value: Option<&Value>| {
                    let words: usize = texts(value).iter().map(|t| t.split_whitespace().count()).sum();
                    if words > *max {
                        let whose = record.map(|r| record_label(resolved, r)).unwrap_or_else(|| "document".into());
                        let text = message.clone().unwrap_or_else(|| format!("{field} has {words} words; the limit is {max}"));
                        findings.push(finding(*level, format!("{whose}: {text}"), record, Some(field)));
                    }
                };
                if kind == DOCUMENT_KIND {
                    check(None, document_fields.get(field));
                } else {
                    for record in of_kind(kind) {
                        check(Some(record), record.fields.get(field));
                    }
                }
            }
            Lint::MaxItems { section, max, level, message } => {
                let count = resolved
                    .sections
                    .iter()
                    .find(|s| s.name == *section)
                    .map_or(0, |s| s.records.len());
                if count > *max {
                    let text = message
                        .clone()
                        .unwrap_or_else(|| format!("section `{section}` has {count} items; the limit is {max}"));
                    findings.push(finding(*level, text, None, None));
                }
            }
        }
    }
    findings
}

pub fn errors(findings: &[LintFinding]) -> usize {
    findings.iter().filter(|f| f.level == Level::Error).count()
}

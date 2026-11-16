//! Templates are gated like type files: hoja renders only the copies the
//! person accepted, and a document's content hash covers every accepted file
//! its template can reach, so an approved document's PDF can't change while
//! it stays approved.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::{Arc, LazyLock};

use regex::Regex;
use serde_json::{Map, Value};

use crate::config::sha256_hex;

/// hoja's own Typst helpers, written into every render root. Part of the
/// binary, so a workspace can't shadow it.
pub const BUILTIN_PATH: &str = "hoja.typ";
const BUILTIN_BODY: &str = include_str!("hoja.typ");

/// Names the render root uses for itself; no workspace file may take them.
pub const RESERVED: &[&str] = &[BUILTIN_PATH, "data.json"];

#[derive(Debug, Clone)]
pub struct TemplateFile {
    pub hash: String,
    pub body: Arc<Vec<u8>>,
}

impl TemplateFile {
    pub fn new(body: Vec<u8>) -> Self {
        Self {
            hash: sha256_hex(&body),
            body: Arc::new(body),
        }
    }
}

/// A set of template files by path relative to `templates/`, with hoja's
/// built-in helpers always present.
#[derive(Debug, Clone)]
pub struct TemplateSet {
    files: BTreeMap<String, TemplateFile>,
}

impl Default for TemplateSet {
    fn default() -> Self {
        Self::new(BTreeMap::new())
    }
}

impl TemplateSet {
    pub fn new(mut files: BTreeMap<String, TemplateFile>) -> Self {
        files.insert(
            BUILTIN_PATH.into(),
            TemplateFile::new(BUILTIN_BODY.as_bytes().to_vec()),
        );
        Self { files }
    }

    pub fn get(&self, path: &str) -> Option<&TemplateFile> {
        self.files.get(path)
    }

    pub fn insert(&mut self, path: &str, file: TemplateFile) {
        if path != BUILTIN_PATH {
            self.files.insert(path.into(), file);
        }
    }

    /// This set with one file replaced, for previewing a change before accepting it.
    pub fn with(&self, path: &str, file: TemplateFile) -> Self {
        let mut next = self.clone();
        next.insert(path, file);
        next
    }

    /// Every file `entry` can reach: the entry, then any file of this set
    /// named by a string literal in a reachable `.typ` file. Rendering gets
    /// exactly these files, so a template can't read anything the hash
    /// doesn't cover; a path built at runtime simply fails to compile.
    pub fn closure(&self, entry: &str) -> Result<BTreeMap<String, TemplateFile>, String> {
        if !self.files.contains_key(entry) {
            return Err(format!(
                "templates/{entry} has not been accepted; review it under Types"
            ));
        }
        let mut found = BTreeMap::new();
        let mut queue = vec![entry.to_string()];
        while let Some(path) = queue.pop() {
            if found.contains_key(&path) {
                continue;
            }
            let Some(file) = self.files.get(&path) else {
                continue;
            };
            if path.ends_with(".typ")
                && let Ok(text) = std::str::from_utf8(&file.body)
            {
                for literal in string_literals(text) {
                    if let Some(target) = resolve(&path, &literal)
                        && self.files.contains_key(&target)
                        && !found.contains_key(&target)
                    {
                        queue.push(target);
                    }
                }
            }
            found.insert(path, file.clone());
        }
        Ok(found)
    }

    /// What a document's content hash records about its template: each
    /// reachable file's hash, or why there is none.
    pub fn fingerprint(&self, entry: Option<&str>) -> Value {
        let Some(entry) = entry else {
            return Value::Null;
        };
        match self.closure(entry) {
            Ok(files) => {
                let map: Map<String, Value> = files
                    .into_iter()
                    .map(|(path, file)| (path, Value::String(file.hash)))
                    .collect();
                serde_json::json!({ "entry": entry, "files": map })
            }
            Err(_) => serde_json::json!({ "entry": entry, "missing": true }),
        }
    }
}

static LITERAL: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r#""((?:[^"\\\n]|\\.)*)""#).expect("valid regex"));

fn string_literals(text: &str) -> impl Iterator<Item = String> + '_ {
    LITERAL
        .captures_iter(text)
        .map(|c| c[1].replace("\\\"", "\"").replace("\\\\", "\\"))
}

/// A literal as a path inside the root, the way Typst resolves it: relative
/// to the file that names it, or to the root when it starts with `/`.
fn resolve(from: &str, literal: &str) -> Option<String> {
    if literal.is_empty() || literal.contains(':') || literal.contains('\\') {
        return None;
    }
    let mut parts: Vec<&str> = if literal.starts_with('/') {
        Vec::new()
    } else {
        from.split('/').collect::<Vec<_>>()[..from.matches('/').count()].to_vec()
    };
    for segment in literal.split('/') {
        match segment {
            "" | "." => {}
            ".." => {
                parts.pop()?;
            }
            other => parts.push(other),
        }
    }
    (!parts.is_empty()).then(|| parts.join("/"))
}

/// A file under `templates/` as it is on disk right now.
#[derive(Debug, Clone)]
pub struct DiskTemplate {
    pub path: String,
    pub file: TemplateFile,
    /// Why this file can't be accepted, such as a reserved name.
    pub error: Option<String>,
}

const MAX_TEMPLATE_BYTES: u64 = 8 * 1024 * 1024;
const MAX_TEMPLATE_FILES: usize = 500;

/// Reads every regular file under `dir`, skipping dot files.
pub fn read_dir(dir: &std::path::Path) -> anyhow::Result<Vec<DiskTemplate>> {
    use anyhow::{Context, bail};
    let mut out = Vec::new();
    if !dir.exists() {
        return Ok(out);
    }
    let mut stack = vec![(dir.to_path_buf(), String::new())];
    while let Some((path, prefix)) = stack.pop() {
        for entry in
            std::fs::read_dir(&path).with_context(|| format!("reading {}", path.display()))?
        {
            let entry = entry?;
            let name = entry.file_name().to_string_lossy().to_string();
            if name.starts_with('.') {
                continue;
            }
            let relative = if prefix.is_empty() {
                name.clone()
            } else {
                format!("{prefix}/{name}")
            };
            let kind = entry.file_type()?;
            if kind.is_dir() {
                stack.push((entry.path(), relative));
            } else if kind.is_file() {
                let size = entry.metadata()?.len();
                if size > MAX_TEMPLATE_BYTES {
                    bail!("templates/{relative} is larger than 8 MiB");
                }
                let body = std::fs::read(entry.path())
                    .with_context(|| format!("reading templates/{relative}"))?;
                let error = RESERVED
                    .contains(&relative.as_str())
                    .then(|| format!("`{relative}` is reserved for hoja's own files; rename it"));
                out.push(DiskTemplate {
                    path: relative,
                    file: TemplateFile::new(body),
                    error,
                });
                if out.len() > MAX_TEMPLATE_FILES {
                    bail!("templates/ holds more than {MAX_TEMPLATE_FILES} files");
                }
            }
        }
    }
    out.sort_by(|a, b| a.path.cmp(&b.path));
    Ok(out)
}

/// Paths named in `set` that `entry` reaches, for telling the person which
/// types a template change affects.
pub fn reaches(set: &TemplateSet, entry: &str, path: &str) -> bool {
    set.closure(entry)
        .map(|files| files.contains_key(path))
        .unwrap_or(false)
}

/// Every path in a closure, sorted; handy for messages and tests.
pub fn paths(files: &BTreeMap<String, TemplateFile>) -> BTreeSet<String> {
    files.keys().cloned().collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn set(files: &[(&str, &str)]) -> TemplateSet {
        TemplateSet::new(
            files
                .iter()
                .map(|(p, b)| (p.to_string(), TemplateFile::new(b.as_bytes().to_vec())))
                .collect(),
        )
    }

    #[test]
    fn closure_follows_literals_relative_to_each_file() {
        let s = set(&[
            (
                "report.typ",
                "#import \"lib/style.typ\": *\n#image(\"/logo.png\")",
            ),
            (
                "lib/style.typ",
                "#import \"../shared.typ\"\n#import \"/hoja.typ\": md",
            ),
            ("shared.typ", "// nothing"),
            ("logo.png", "png"),
            ("other.typ", "#let x = 1"),
        ]);
        let reached = paths(&s.closure("report.typ").unwrap());
        let expected: BTreeSet<String> = [
            "report.typ",
            "lib/style.typ",
            "shared.typ",
            "logo.png",
            "hoja.typ",
        ]
        .into_iter()
        .map(String::from)
        .collect();
        assert_eq!(reached, expected);
        assert!(s.closure("missing.typ").is_err());
    }

    #[test]
    fn literals_cannot_escape_the_root() {
        assert_eq!(resolve("a.typ", "../x.typ"), None);
        assert_eq!(resolve("d/a.typ", "../x.typ"), Some("x.typ".into()));
        assert_eq!(resolve("d/a.typ", "/x.typ"), Some("x.typ".into()));
        assert_eq!(resolve("a.typ", "@preview/pkg:0.1.0"), None);
    }

    #[test]
    fn fingerprint_changes_only_with_reachable_files() {
        let a = set(&[
            ("r.typ", "#import \"s.typ\""),
            ("s.typ", "1"),
            ("u.typ", "1"),
        ]);
        let b = a.with("u.typ", TemplateFile::new(b"2".to_vec()));
        let c = a.with("s.typ", TemplateFile::new(b"2".to_vec()));
        assert_eq!(a.fingerprint(Some("r.typ")), b.fingerprint(Some("r.typ")));
        assert_ne!(a.fingerprint(Some("r.typ")), c.fingerprint(Some("r.typ")));
        assert_eq!(a.fingerprint(None), Value::Null);
    }
}

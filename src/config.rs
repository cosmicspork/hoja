//! Document types: parsed from `types/*.toml` in a workspace, validated, and
//! applied to field values. A type only takes effect once the person accepts
//! its exact contents (see `Store::accept_type`).

use std::path::{Path, PathBuf};

use anyhow::{Context, bail};
use indexmap::IndexMap;
use regex::Regex;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use sha2::{Digest, Sha256};

const MAX_STRING: usize = 500;
const MAX_TEXT: usize = 20_000;
const MAX_LIST: usize = 50;
const RESERVED_FIELDS: &[&str] = &["id", "kind", "type", "version", "reviewed"];
/// Lints and `where` filters use this kind name to address document fields.
pub const DOCUMENT_KIND: &str = "document";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FirstDraft {
    Human,
    Agent,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum Format {
    Pdf,
    Md,
    Html,
}

impl Format {
    pub fn extension(self) -> &'static str {
        match self {
            Self::Pdf => "pdf",
            Self::Md => "md",
            Self::Html => "html",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FieldType {
    String,
    Text,
    Tags,
    List,
    Enum,
    Date,
    Bool,
    Ref,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FieldConfig {
    #[serde(rename = "type")]
    pub ty: FieldType,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub help: Option<String>,
    #[serde(default)]
    pub required: bool,
    /// Agents may neither write nor suggest this field.
    #[serde(default)]
    pub person_only: bool,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub options: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ref_kind: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct KindConfig {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    /// The field shown when a record is listed or referenced.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
    /// Setting this to false sends every agent write to records of this kind
    /// through a card, even where the type lets agents draft directly.
    #[serde(default = "yes")]
    pub agent_direct: bool,
    /// The field in which an agent cites where a new record's facts come
    /// from. Setting it lets agents propose new records of this kind, always
    /// as a card and only with this field filled, even in a type whose first
    /// draft is the person's: facts (a degree, an employer) are looked up,
    /// not written, so the person checks the source instead of typing them.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub proposal_source: Option<String>,
    pub fields: IndexMap<String, FieldConfig>,
}

impl KindConfig {
    /// Whether `patch` fills this kind's `proposal_source` field. False for a
    /// kind without one.
    pub fn cites(&self, patch: &Map<String, Value>) -> bool {
        let Some(name) = &self.proposal_source else {
            return false;
        };
        match (self.fields.get(name), patch.get(name)) {
            (Some(field), Some(value)) => {
                matches!(normalize_value(name, field, value), Ok(Some(_)))
            }
            _ => false,
        }
    }

    pub fn label_field(&self) -> Option<&str> {
        self.label.as_deref().or_else(|| {
            self.fields
                .iter()
                .find(|(_, f)| matches!(f.ty, FieldType::String | FieldType::Text))
                .map(|(name, _)| name.as_str())
        })
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SectionConfig {
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    /// Select records of another accepted type (a status report picking
    /// from the RAID log). Those records keep their own type's gates.
    #[serde(rename = "type", default, skip_serializing_if = "Option::is_none")]
    pub from_type: Option<String>,
    pub kind: String,
    /// Only records whose fields equal these values may be selected here.
    #[serde(rename = "where", default, skip_serializing_if = "IndexMap::is_empty")]
    pub filter: IndexMap<String, String>,
    /// A `ref` field of `kind`; rendering groups items under the referenced record.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub group_by: Option<String>,
    /// Markdown/HTML line for one item, e.g. `"{text} ({refs})"`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub item: Option<String>,
    /// `all` holds every live record of `kind` matching `where`, instead of a
    /// hand-picked selection, so a log can't silently leave an entry out.
    #[serde(default, skip_serializing_if = "Include::is_selected")]
    pub include: Include,
    /// For an `all` section: the field records are ordered by, then creation.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sort: Option<String>,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Include {
    #[default]
    Selected,
    All,
}

impl Include {
    fn is_selected(&self) -> bool {
        *self == Self::Selected
    }
}

impl SectionConfig {
    pub fn includes_all(&self) -> bool {
        self.include == Include::All
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DocumentConfig {
    #[serde(default)]
    pub fields: IndexMap<String, FieldConfig>,
    #[serde(default)]
    pub sections: Vec<SectionConfig>,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Level {
    #[default]
    Error,
    Warn,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "rule", rename_all = "snake_case", deny_unknown_fields)]
pub enum Lint {
    /// No text field of a selected record (or the document) may match `pattern`.
    Forbid {
        pattern: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        kind: Option<String>,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        fields: Vec<String>,
        #[serde(default)]
        level: Level,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        message: Option<String>,
    },
    Require {
        kind: String,
        field: String,
        #[serde(default)]
        level: Level,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        message: Option<String>,
    },
    /// Every entry of `field` matches `pattern`, and there are at least `min`.
    Match {
        kind: String,
        field: String,
        pattern: String,
        #[serde(default)]
        min: usize,
        #[serde(default)]
        level: Level,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        message: Option<String>,
    },
    MaxWords {
        kind: String,
        field: String,
        max: usize,
        #[serde(default)]
        level: Level,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        message: Option<String>,
    },
    MaxItems {
        section: String,
        max: usize,
        #[serde(default)]
        level: Level,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        message: Option<String>,
    },
    /// Every selected record of `kind` is marked reviewed by the person.
    Reviewed {
        kind: String,
        #[serde(default)]
        level: Level,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        message: Option<String>,
    },
    /// No two selected records of `kind` share a value of `field`, e.g. one
    /// bullet per accomplishment when several are written different ways.
    Unique {
        kind: String,
        field: String,
        #[serde(default)]
        level: Level,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        message: Option<String>,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TypeConfig {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    pub first_draft: FirstDraft,
    /// Setting this to false sends every agent write through a card. There is
    /// deliberately no setting that lets agents skip a card the core requires.
    #[serde(default = "yes")]
    pub agent_direct: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub template: Option<String>,
    #[serde(default = "default_filename")]
    pub filename: String,
    #[serde(default = "default_exports")]
    pub exports: Vec<Format>,
    pub kinds: IndexMap<String, KindConfig>,
    #[serde(default)]
    pub document: DocumentConfig,
    #[serde(default)]
    pub lints: Vec<Lint>,
}

fn yes() -> bool {
    true
}
fn default_filename() -> String {
    "{owner}_{type}_{title}".into()
}
fn default_exports() -> Vec<Format> {
    vec![Format::Pdf]
}

fn valid_name(name: &str, separator: char) -> bool {
    let mut chars = name.chars();
    chars.next().is_some_and(|c| c.is_ascii_lowercase())
        && name.len() <= 40
        && chars.all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == separator)
}

pub fn valid_type_name(name: &str) -> bool {
    valid_name(name, '-')
}

impl TypeConfig {
    pub fn parse(body: &str) -> anyhow::Result<Self> {
        let config: Self = toml::from_str(body)?;
        config.validate()?;
        Ok(config)
    }

    pub fn kind(&self, kind: &str) -> Option<&KindConfig> {
        self.kinds.get(kind)
    }

    /// The type a section's records come from: its `type`, or this one.
    pub fn section_type<'a>(&self, section: &'a SectionConfig, own: &'a str) -> &'a str {
        section.from_type.as_deref().unwrap_or(own)
    }

    pub fn section(&self, name: &str) -> Option<&SectionConfig> {
        self.document.sections.iter().find(|s| s.name == name)
    }

    /// The fields a kind name addresses, where `document` means the document's own fields.
    pub fn fields_of(&self, kind: &str) -> Option<&IndexMap<String, FieldConfig>> {
        if kind == DOCUMENT_KIND {
            Some(&self.document.fields)
        } else {
            self.kinds.get(kind).map(|k| &k.fields)
        }
    }

    fn validate(&self) -> anyhow::Result<()> {
        if self.kinds.is_empty() {
            bail!("a type needs at least one kind");
        }
        for (name, kind) in &self.kinds {
            if !valid_name(name, '_') || name == DOCUMENT_KIND {
                bail!("invalid kind name `{name}`");
            }
            validate_fields(&format!("kind `{name}`"), &kind.fields, &self.kinds)?;
            if let Some(label) = &kind.label
                && !kind.fields.contains_key(label)
            {
                bail!("kind `{name}` labels itself with unknown field `{label}`");
            }
            if let Some(source) = &kind.proposal_source {
                match kind.fields.get(source).map(|f| f.ty) {
                    Some(FieldType::String | FieldType::Text | FieldType::List) => {}
                    Some(_) => bail!(
                        "kind `{name}`: proposal_source `{source}` must be a string, text or list field"
                    ),
                    None => bail!("kind `{name}`: proposal_source names unknown field `{source}`"),
                }
            }
        }
        validate_fields("document", &self.document.fields, &self.kinds)?;
        let mut seen = Vec::new();
        for section in &self.document.sections {
            if !valid_name(&section.name, '_') || seen.contains(&&section.name) {
                bail!("invalid or duplicate section name `{}`", section.name);
            }
            seen.push(&section.name);
            if section.sort.is_some() && !section.includes_all() {
                bail!(
                    "section `{}` sorts, which only an `include = \"all\"` section can",
                    section.name
                );
            }
            if let Some(other) = &section.from_type {
                // The other type is checked when a record is selected, since
                // it is accepted separately and may change on its own.
                if !valid_type_name(other) || !valid_name(&section.kind, '_') {
                    bail!("section `{}` names an invalid type or kind", section.name);
                }
                if section.group_by.is_some() {
                    bail!(
                        "section `{}` selects from another type, so it can't group_by",
                        section.name
                    );
                }
                continue;
            }
            let kind = self.kinds.get(&section.kind).with_context(|| {
                format!(
                    "section `{}`: unknown kind `{}`",
                    section.name, section.kind
                )
            })?;
            for field in section.filter.keys() {
                if !kind.fields.contains_key(field) {
                    bail!(
                        "section `{}` filters on unknown field `{field}`",
                        section.name
                    );
                }
            }
            if let Some(sort) = &section.sort
                && !kind.fields.contains_key(sort)
            {
                bail!("section `{}` sorts by unknown field `{sort}`", section.name);
            }
            if let Some(group) = &section.group_by
                && kind
                    .fields
                    .get(group)
                    .is_none_or(|f| f.ty != FieldType::Ref)
            {
                bail!(
                    "section `{}` groups by `{group}`, which is not a ref field",
                    section.name
                );
            }
            if let Some(item) = &section.item {
                for placeholder in placeholders(item) {
                    if !kind.fields.contains_key(placeholder) && placeholder != "id" {
                        bail!(
                            "section `{}` item pattern names unknown field `{placeholder}`",
                            section.name
                        );
                    }
                }
            }
        }
        for lint in &self.lints {
            self.validate_lint(lint)?;
        }
        if let Some(template) = &self.template
            && (template.is_empty() || template.contains(['/', '\\']) || template.starts_with('.'))
        {
            bail!("template must be a file name inside templates/");
        }
        for placeholder in placeholders(&self.filename) {
            let known = matches!(placeholder, "owner" | "type" | "title" | "date")
                || placeholder
                    .strip_prefix("doc.")
                    .is_some_and(|f| self.document.fields.contains_key(f));
            if !known {
                bail!("filename pattern names unknown placeholder `{{{placeholder}}}`");
            }
        }
        if self.exports.is_empty() {
            bail!("exports must name at least one format");
        }
        Ok(())
    }

    fn validate_lint(&self, lint: &Lint) -> anyhow::Result<()> {
        let field_of = |kind: &str, field: &str| -> anyhow::Result<FieldType> {
            self.fields_of(kind)
                .with_context(|| format!("lint names unknown kind `{kind}`"))?
                .get(field)
                .map(|f| f.ty)
                .with_context(|| format!("lint names unknown field `{kind}.{field}`"))
        };
        match lint {
            Lint::Forbid {
                pattern,
                kind,
                fields,
                ..
            } => {
                Regex::new(pattern).with_context(|| format!("invalid lint pattern `{pattern}`"))?;
                if let Some(kind) = kind {
                    for field in fields {
                        field_of(kind, field)?;
                    }
                    if fields.is_empty() {
                        self.fields_of(kind)
                            .with_context(|| format!("lint names unknown kind `{kind}`"))?;
                    }
                }
            }
            Lint::Require { kind, field, .. } => {
                field_of(kind, field)?;
            }
            Lint::Match {
                kind,
                field,
                pattern,
                ..
            } => {
                field_of(kind, field)?;
                Regex::new(pattern).with_context(|| format!("invalid lint pattern `{pattern}`"))?;
            }
            Lint::MaxWords { kind, field, .. } => {
                let ty = field_of(kind, field)?;
                if !matches!(ty, FieldType::String | FieldType::Text) {
                    bail!("max_words applies to string or text fields");
                }
            }
            Lint::MaxItems { section, .. } => {
                if self.section(section).is_none() {
                    bail!("lint names unknown section `{section}`");
                }
            }
            Lint::Reviewed { kind, .. } => {
                if self.kind(kind).is_none() {
                    bail!("lint names unknown kind `{kind}`");
                }
            }
            Lint::Unique { kind, field, .. } => {
                if kind == DOCUMENT_KIND {
                    bail!("unique applies to record kinds");
                }
                field_of(kind, field)?;
            }
        }
        Ok(())
    }
}

fn validate_fields(
    owner: &str,
    fields: &IndexMap<String, FieldConfig>,
    kinds: &IndexMap<String, KindConfig>,
) -> anyhow::Result<()> {
    for (name, field) in fields {
        if !valid_name(name, '_') || RESERVED_FIELDS.contains(&name.as_str()) {
            bail!("{owner}: invalid field name `{name}`");
        }
        match field.ty {
            FieldType::Enum if field.options.is_empty() => {
                bail!("{owner}: enum field `{name}` needs options")
            }
            FieldType::Ref => {
                let target = field
                    .ref_kind
                    .as_deref()
                    .with_context(|| format!("{owner}: ref field `{name}` needs ref_kind"))?;
                if !kinds.contains_key(target) {
                    bail!("{owner}: ref field `{name}` points at unknown kind `{target}`");
                }
            }
            _ => {}
        }
        if field.ty != FieldType::Enum && !field.options.is_empty() {
            bail!("{owner}: only enum fields take options (`{name}`)");
        }
        if field.ty != FieldType::Ref && field.ref_kind.is_some() {
            bail!("{owner}: only ref fields take ref_kind (`{name}`)");
        }
    }
    Ok(())
}

/// `{name}` placeholders in a pattern, in order.
pub fn placeholders(pattern: &str) -> impl Iterator<Item = &str> {
    pattern
        .split('{')
        .skip(1)
        .filter_map(|s| s.split_once('}').map(|(p, _)| p))
}

/// Normalizes one submitted value. `None` means the field is cleared.
pub fn normalize_value(
    name: &str,
    field: &FieldConfig,
    value: &Value,
) -> Result<Option<Value>, String> {
    let as_string = || -> Result<Option<String>, String> {
        match value {
            Value::Null => Ok(None),
            Value::String(s) if s.trim().is_empty() => Ok(None),
            Value::String(s) => Ok(Some(s.clone())),
            _ => Err(format!("`{name}` must be a string")),
        }
    };
    let as_list = || -> Result<Option<Vec<String>>, String> {
        let items = match value {
            Value::Null => return Ok(None),
            Value::Array(items) => items,
            _ => return Err(format!("`{name}` must be a list of strings")),
        };
        let mut out: Vec<String> = Vec::new();
        for item in items {
            let Value::String(s) = item else {
                return Err(format!("`{name}` must be a list of strings"));
            };
            let s = s.trim();
            if s.is_empty() {
                continue;
            }
            if s.chars().count() > MAX_STRING {
                return Err(format!(
                    "an entry of `{name}` is longer than {MAX_STRING} characters"
                ));
            }
            out.push(s.to_string());
        }
        if out.len() > MAX_LIST {
            return Err(format!("`{name}` has more than {MAX_LIST} entries"));
        }
        Ok((!out.is_empty()).then_some(out))
    };
    let normalized = match field.ty {
        FieldType::String | FieldType::Ref => match as_string()? {
            Some(s) if s.contains('\n') => return Err(format!("`{name}` must be a single line")),
            Some(s) if s.chars().count() > MAX_STRING => {
                return Err(format!("`{name}` is longer than {MAX_STRING} characters"));
            }
            other => other.map(Value::String),
        },
        FieldType::Text => match as_string()? {
            Some(s) if s.chars().count() > MAX_TEXT => {
                return Err(format!("`{name}` is longer than {MAX_TEXT} characters"));
            }
            other => other.map(Value::String),
        },
        FieldType::Enum => match as_string()? {
            Some(s) if !field.options.contains(&s) => {
                return Err(format!(
                    "`{name}` must be one of: {}",
                    field.options.join(", ")
                ));
            }
            other => other.map(Value::String),
        },
        FieldType::Date => match as_string()? {
            Some(s) if !valid_date(&s) => {
                return Err(format!("`{name}` must be YYYY, YYYY-MM or YYYY-MM-DD"));
            }
            other => other.map(Value::String),
        },
        FieldType::Bool => match value {
            Value::Null => None,
            Value::Bool(b) => Some(Value::Bool(*b)),
            _ => return Err(format!("`{name}` must be true or false")),
        },
        FieldType::List => as_list()?.map(Value::from),
        FieldType::Tags => as_list()?.map(|l| {
            let mut tags: Vec<String> = Vec::new();
            for tag in l {
                let tag = tag.to_lowercase();
                if !tags.contains(&tag) {
                    tags.push(tag);
                }
            }
            Value::from(tags)
        }),
    };
    Ok(normalized)
}

fn valid_date(s: &str) -> bool {
    let full = match s.len() {
        4 => format!("{s}-01-01"),
        7 => format!("{s}-01"),
        10 => s.to_string(),
        _ => return false,
    };
    chrono::NaiveDate::parse_from_str(&full, "%Y-%m-%d").is_ok()
}

/// Applies `patch` to `current` against `fields`: unknown names are rejected,
/// null or empty values clear a field. Required fields are checked afterwards.
pub fn apply_patch(
    fields: &IndexMap<String, FieldConfig>,
    current: &Map<String, Value>,
    patch: &Map<String, Value>,
) -> Result<Map<String, Value>, String> {
    let mut next = current.clone();
    for (name, value) in patch {
        let field = fields
            .get(name)
            .ok_or_else(|| format!("unknown field `{name}`"))?;
        match normalize_value(name, field, value)? {
            Some(value) => {
                next.insert(name.clone(), value);
            }
            None => {
                next.shift_remove(name);
            }
        }
    }
    // Keep the declared field order so snapshots and diffs are stable.
    let mut ordered = Map::new();
    for name in fields.keys() {
        if let Some(value) = next.shift_remove(name) {
            ordered.insert(name.clone(), value);
        }
    }
    // Fields a later type revision removed stay with the record; they are not ours to drop.
    ordered.extend(next);
    for (name, field) in fields {
        if field.required && !ordered.contains_key(name) {
            return Err(format!("`{name}` is required"));
        }
    }
    Ok(ordered)
}

pub fn sha256_hex(bytes: &[u8]) -> String {
    Sha256::digest(bytes)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

/// JSON with every object's keys sorted, for hashing.
pub fn canonical_json(value: &Value) -> String {
    fn sorted(value: &Value) -> Value {
        match value {
            Value::Object(map) => {
                let mut keys: Vec<&String> = map.keys().collect();
                keys.sort();
                Value::Object(
                    keys.into_iter()
                        .map(|k| (k.clone(), sorted(&map[k])))
                        .collect(),
                )
            }
            Value::Array(items) => Value::Array(items.iter().map(sorted).collect()),
            other => other.clone(),
        }
    }
    sorted(value).to_string()
}

/// A type file as it is on disk right now.
#[derive(Debug, Clone)]
pub struct TypeFile {
    pub name: String,
    pub body: String,
    pub hash: String,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WorkspaceConfig {
    #[serde(default)]
    pub owner: Option<String>,
}

#[derive(Debug, Clone)]
pub struct Workspace {
    pub root: PathBuf,
    pub config: WorkspaceConfig,
}

impl Workspace {
    pub fn open(root: &Path) -> anyhow::Result<Self> {
        let root = root
            .canonicalize()
            .with_context(|| format!("workspace {} does not exist", root.display()))?;
        let config_path = root.join("hoja.toml");
        let config = if config_path.exists() {
            let body = std::fs::read_to_string(&config_path).context("reading hoja.toml")?;
            toml::from_str(&body).context("parsing hoja.toml")?
        } else {
            WorkspaceConfig::default()
        };
        Ok(Self { root, config })
    }

    pub fn database(&self) -> PathBuf {
        self.root.join("hoja.sqlite3")
    }

    pub fn templates(&self) -> PathBuf {
        self.root.join("templates")
    }

    /// `samples/<type>.json`: invented content for previewing a template
    /// before accepting it. Optional; hoja makes placeholders without it.
    pub fn sample(&self, type_name: &str) -> anyhow::Result<Option<Value>> {
        if !valid_type_name(type_name) {
            bail!("invalid type name `{type_name}`");
        }
        let path = self.root.join("samples").join(format!("{type_name}.json"));
        if !path.exists() {
            return Ok(None);
        }
        let body = std::fs::read_to_string(&path)
            .with_context(|| format!("reading samples/{type_name}.json"))?;
        let value = serde_json::from_str(&body)
            .with_context(|| format!("parsing samples/{type_name}.json"))?;
        Ok(Some(value))
    }

    /// Every file under `templates/` as it is on disk now.
    pub fn template_files(&self) -> anyhow::Result<Vec<crate::templates::DiskTemplate>> {
        crate::templates::read_dir(&self.templates())
    }

    pub fn out(&self) -> PathBuf {
        self.root.join("out")
    }

    /// Reads `types/*.toml`. Files with invalid names are reported as errors
    /// rather than skipped, so a typo doesn't silently hide a type.
    pub fn type_files(&self) -> anyhow::Result<Vec<TypeFile>> {
        let dir = self.root.join("types");
        if !dir.exists() {
            return Ok(Vec::new());
        }
        let mut files = Vec::new();
        for entry in std::fs::read_dir(&dir).context("reading types/")? {
            let path = entry?.path();
            if path.extension().and_then(|e| e.to_str()) != Some("toml") {
                continue;
            }
            let name = path
                .file_stem()
                .and_then(|s| s.to_str())
                .unwrap_or_default()
                .to_string();
            if !valid_type_name(&name) {
                bail!(
                    "type file name `{}` must be lowercase letters, digits and dashes",
                    path.display()
                );
            }
            let body = std::fs::read_to_string(&path)
                .with_context(|| format!("reading {}", path.display()))?;
            let hash = sha256_hex(body.as_bytes());
            files.push(TypeFile { name, body, hash });
        }
        files.sort_by(|a, b| a.name.cmp(&b.name));
        Ok(files)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    const MINIMAL: &str = r#"
first_draft = "human"
[kinds.note.fields.text]
type = "text"
required = true
"#;

    #[test]
    fn canonical_json_ignores_key_order() {
        let a = json!({"b": 1, "a": {"y": [1, {"d": 1, "c": 2}], "x": null}});
        let b = json!({"a": {"x": null, "y": [1, {"c": 2, "d": 1}]}, "b": 1});
        assert_ne!(a.to_string(), b.to_string());
        assert_eq!(canonical_json(&a), canonical_json(&b));
    }

    #[test]
    fn parses_a_minimal_type() {
        let config = TypeConfig::parse(MINIMAL).unwrap();
        assert!(config.agent_direct);
        assert_eq!(config.exports, vec![Format::Pdf]);
        assert_eq!(config.kind("note").unwrap().label_field(), Some("text"));
    }

    #[test]
    fn a_proposal_cites_only_with_a_filled_source() {
        let config = TypeConfig::parse(
            "first_draft = \"human\"\n[kinds.degree]\nproposal_source = \"sources\"\n\
             [kinds.degree.fields.field]\ntype = \"string\"\n\
             [kinds.degree.fields.sources]\ntype = \"list\"\n\
             [kinds.note.fields.text]\ntype = \"text\"",
        )
        .unwrap();
        let degree = config.kind("degree").unwrap();
        let patch = |v: serde_json::Value| v.as_object().unwrap().clone();
        assert!(degree.cites(&patch(json!({"sources": ["transcript.pdf p.1"]}))));
        for missing in [
            json!({"field": "Entrepreneurship"}),
            json!({"sources": []}),
            json!({"sources": ["  "]}),
            json!({"sources": null}),
            json!({"sources": "not a list"}),
        ] {
            assert!(!degree.cites(&patch(missing.clone())), "{missing}");
        }
        assert!(
            !config
                .kind("note")
                .unwrap()
                .cites(&patch(json!({"text": "x"})))
        );
    }

    #[test]
    fn rejects_unknown_keys_and_bad_references() {
        for body in [
            "first_draft = \"human\"\nloosen = true\n[kinds.a.fields.t]\ntype = \"text\"",
            "first_draft = \"human\"\n[kinds.a.fields.r]\ntype = \"ref\"\nref_kind = \"nope\"",
            "first_draft = \"human\"\n[kinds.a.fields.e]\ntype = \"enum\"",
            "first_draft = \"human\"\n[kinds.a.fields.reviewed]\ntype = \"bool\"",
            "first_draft = \"human\"\ntemplate = \"../x.typ\"\n[kinds.a.fields.t]\ntype = \"text\"",
            "first_draft = \"human\"\nfilename = \"{doc.nope}\"\n[kinds.a.fields.t]\ntype = \"text\"",
            "first_draft = \"human\"\n[kinds.a.fields.t]\ntype = \"text\"\n[[lints]]\nrule = \"forbid\"\npattern = \"(\"",
            "first_draft = \"human\"\n[kinds.a.fields.t]\ntype = \"text\"\n[[lints]]\nrule = \"require\"\nkind = \"a\"\nfield = \"x\"",
            "first_draft = \"human\"\n[kinds.a.fields.t]\ntype = \"text\"\n[[lints]]\nrule = \"max_items\"\nsection = \"x\"\nmax = 1\nextra = 2",
            "first_draft = \"human\"\n[kinds.a.fields.t]\ntype = \"text\"\n[[document.sections]]\nname = \"s\"\nkind = \"a\"\nsort = \"t\"",
            "first_draft = \"human\"\n[kinds.a.fields.t]\ntype = \"text\"\n[[document.sections]]\nname = \"s\"\nkind = \"a\"\ninclude = \"all\"\nsort = \"nope\"",
            "first_draft = \"human\"\n[kinds.a.fields.t]\ntype = \"text\"\n[[document.sections]]\nname = \"s\"\nkind = \"a\"\ninclude = \"some\"",
            "first_draft = \"human\"\n[kinds.a]\nproposal_source = \"nope\"\n[kinds.a.fields.t]\ntype = \"text\"",
            "first_draft = \"human\"\n[kinds.a]\nproposal_source = \"d\"\n[kinds.a.fields.d]\ntype = \"date\"",
        ] {
            assert!(TypeConfig::parse(body).is_err(), "{body}");
        }
    }

    #[test]
    fn patches_normalize_values_and_check_required() {
        let config = TypeConfig::parse(
            r#"
first_draft = "agent"
[kinds.item.fields.text]
type = "string"
required = true
[kinds.item.fields.tags]
type = "tags"
[kinds.item.fields.when]
type = "date"
[kinds.item.fields.status]
type = "enum"
options = ["done", "next"]
"#,
        )
        .unwrap();
        let fields = &config.kind("item").unwrap().fields;
        let patch = json!({"tags": [" Rust ", "rust", ""], "text": "A", "when": "2026-10"});
        let next = apply_patch(fields, &Map::new(), patch.as_object().unwrap()).unwrap();
        assert_eq!(
            Value::Object(next.clone()),
            json!({"text": "A", "tags": ["rust"], "when": "2026-10"})
        );
        let clear = json!({"tags": null});
        let cleared = apply_patch(fields, &next, clear.as_object().unwrap()).unwrap();
        assert!(!cleared.contains_key("tags"));
        for bad in [
            json!({"text": ""}),
            json!({"text": "a\nb"}),
            json!({"status": "maybe"}),
            json!({"when": "Oct 2026"}),
            json!({"nope": "x"}),
        ] {
            assert!(
                apply_patch(fields, &next, bad.as_object().unwrap()).is_err(),
                "{bad}"
            );
        }
    }
}

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, MutexGuard, RwLock};

use anyhow::{Context, anyhow};
use rusqlite::{Connection, OptionalExtension, Row, params};
use serde_json::{Map, Value, json};
use uuid::Uuid;

use crate::config::{
    FieldType, KindConfig, SectionConfig, TypeConfig, TypeFile, apply_patch, canonical_json,
    sha256_hex,
};
use crate::gates::{self, Actor, Author, Op, Route, Write};
use crate::lint;
use crate::model::{
    Action, Approval, Attribution, CreateDocument, CreateRecord, Decide, Decision, Document,
    Outcome, Record, Revision, Section, SelectSection, TemplateChange, TemplateStatus, TypeChange,
    TypeStatus, UpdateDocument, UpdateRecord,
};
use crate::provenance::{self, Provenance};
use crate::templates::{self, DiskTemplate, TemplateFile, TemplateSet};

pub struct Store {
    connection: Mutex<Connection>,
    /// Accepted type configs. Only `accept_type` changes this.
    types: RwLock<HashMap<String, Arc<TypeConfig>>>,
    /// Accepted template files. Only `accept_template` changes this.
    templates: RwLock<TemplateSet>,
}

#[derive(Debug)]
pub enum StoreError {
    Invalid(String),
    /// A gate refused the write; no card was opened.
    Refused(String),
    NotFound,
    Conflict,
    /// The card's target changed since the agent read it.
    Stale,
    Internal(String),
}

impl std::fmt::Display for StoreError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Invalid(message) | Self::Refused(message) | Self::Internal(message) => {
                f.write_str(message)
            }
            Self::NotFound => f.write_str("not found"),
            Self::Conflict => f.write_str("changed by another request; reload before saving"),
            Self::Stale => f.write_str("the target changed since this card was opened"),
        }
    }
}
impl std::error::Error for StoreError {}

fn internal(err: impl std::fmt::Display) -> StoreError {
    StoreError::Internal(err.to_string())
}

const MAX_SELECTION: usize = 200;
const MAX_NOTE: usize = 2_000;

const MIGRATIONS: &[&str] = &["
CREATE TABLE type_configs(name TEXT PRIMARY KEY, accepted_hash TEXT NOT NULL, accepted_body TEXT NOT NULL, accepted_at TEXT NOT NULL);
CREATE TABLE records(id TEXT PRIMARY KEY, type TEXT NOT NULL, kind TEXT NOT NULL, fields_json TEXT NOT NULL, reviewed INTEGER NOT NULL DEFAULT 0 CHECK(reviewed IN (0,1)), reviewed_at TEXT, version INTEGER NOT NULL DEFAULT 1 CHECK(version>0), created_at TEXT NOT NULL, updated_at TEXT NOT NULL, deleted_at TEXT);
CREATE INDEX records_type_kind ON records(type, kind);
CREATE TABLE documents(id TEXT PRIMARY KEY, type TEXT NOT NULL, title TEXT NOT NULL, fields_json TEXT NOT NULL, approved_hash TEXT, approved_at TEXT, version INTEGER NOT NULL DEFAULT 1 CHECK(version>0), created_at TEXT NOT NULL, updated_at TEXT NOT NULL);
CREATE TABLE document_items(document_id TEXT NOT NULL REFERENCES documents(id) ON DELETE CASCADE, section TEXT NOT NULL, position INTEGER NOT NULL, record_id TEXT NOT NULL REFERENCES records(id), PRIMARY KEY(document_id, section, position), UNIQUE(document_id, section, record_id));
CREATE TABLE revisions(id INTEGER PRIMARY KEY AUTOINCREMENT, target_kind TEXT NOT NULL CHECK(target_kind IN ('record','document')), target_id TEXT NOT NULL, version INTEGER NOT NULL, author TEXT NOT NULL CHECK(author IN ('person','agent','agent_accepted')), agent TEXT, approval_id TEXT REFERENCES approvals(id), edited INTEGER NOT NULL DEFAULT 0 CHECK(edited IN (0,1)), change TEXT NOT NULL, snapshot_json TEXT NOT NULL, created_at TEXT NOT NULL, UNIQUE(target_kind, target_id, version), CHECK((author='person') = (agent IS NULL)), CHECK((author='agent_accepted') = (approval_id IS NOT NULL)));
CREATE TABLE approvals(id TEXT PRIMARY KEY, request_key TEXT NOT NULL, action TEXT NOT NULL CHECK(action IN ('record.create','record.update','document.create','document.update','document.select')), type TEXT NOT NULL, target_id TEXT, base_version INTEGER, payload_json TEXT NOT NULL, agent TEXT NOT NULL, note TEXT NOT NULL DEFAULT '', state TEXT NOT NULL CHECK(state IN ('pending','accepted','rejected','changes_requested')), reason TEXT, edited INTEGER NOT NULL DEFAULT 0 CHECK(edited IN (0,1)), result_json TEXT, created_at TEXT NOT NULL, decided_at TEXT);
CREATE UNIQUE INDEX approvals_pending_key ON approvals(request_key) WHERE state='pending';
CREATE INDEX approvals_state ON approvals(state, created_at);
", "
CREATE TABLE template_files(path TEXT PRIMARY KEY, accepted_hash TEXT NOT NULL, accepted_body BLOB NOT NULL, accepted_at TEXT NOT NULL);
ALTER TABLE documents ADD COLUMN approved_content TEXT;
", "
CREATE TABLE node_cards(card_id TEXT PRIMARY KEY REFERENCES approvals(id), approval_id TEXT NOT NULL UNIQUE, raised_at TEXT NOT NULL);
"];

/// How a write is attributed in its revision.
#[derive(Clone, Copy)]
struct Attrib<'a> {
    author: Author,
    agent: Option<&'a str>,
    approval_id: Option<&'a str>,
    edited: bool,
}

impl<'a> Attrib<'a> {
    fn of(actor: &'a Actor) -> Self {
        match actor {
            Actor::Person => Self {
                author: Author::Person,
                agent: None,
                approval_id: None,
                edited: false,
            },
            Actor::Agent(name) => Self {
                author: Author::Agent,
                agent: Some(name),
                approval_id: None,
                edited: false,
            },
        }
    }
}

/// A document with its selected records resolved, for linting, hashing and rendering.
pub struct Resolved {
    pub config: Arc<TypeConfig>,
    /// Every accepted type a selected record belongs to, including the document's own.
    pub configs: HashMap<String, Arc<TypeConfig>>,
    pub document: Document,
    pub sections: Vec<ResolvedSection>,
    /// Records named by `group_by` refs, by id.
    pub refs: HashMap<String, Record>,
    /// Selected record ids that were deleted since, by section.
    pub missing: Vec<(String, String)>,
    /// The accepted template files that render this document, by hash.
    pub template: Value,
}

pub struct ResolvedSection {
    pub name: String,
    pub records: Vec<Record>,
}

impl Resolved {
    /// The kind a record belongs to, in whichever type it comes from.
    pub fn kind_config(&self, record: &Record) -> Option<&KindConfig> {
        self.configs
            .get(&record.type_name)
            .unwrap_or(&self.config)
            .kind(&record.kind)
    }

    /// The content the person approves: title, fields, every selected and
    /// referenced record's fields in order, the type's layout (section and
    /// field titles, item patterns), and the template files that render it.
    pub fn content(&self) -> Value {
        let sections: Vec<Value> = self
            .sections
            .iter()
            .map(|s| {
                json!({
                    "name": s.name,
                    "items": s.records.iter().map(|r| json!({"id": r.id, "fields": r.fields})).collect::<Vec<_>>(),
                })
            })
            .collect();
        let mut refs: Vec<(&String, &Record)> = self.refs.iter().collect();
        refs.sort_by(|a, b| a.0.cmp(b.0));
        let refs: Vec<Value> = refs
            .into_iter()
            .map(|(id, r)| json!({"id": id, "fields": r.fields}))
            .collect();
        let config = &self.config;
        let layout = json!({
            "title": config.title,
            "sections": config.document.sections.iter().map(|s| json!({
                "name": s.name, "title": s.title, "type": s.from_type, "kind": s.kind,
                "item": s.item, "group_by": s.group_by, "include": s.include, "sort": s.sort,
            })).collect::<Vec<_>>(),
            "fields": config.document.fields.iter().map(|(n, f)| (n.clone(), json!(f.title))).collect::<Map<_, _>>(),
        });
        json!({
            "title": self.document.title,
            "fields": self.document.fields,
            "sections": sections,
            "refs": refs,
            "layout": layout,
            "template": self.template,
        })
    }

    /// Hashes the content with object keys sorted, so storing the same fields
    /// in another order can't change it.
    pub fn content_hash(&self) -> String {
        sha256_hex(canonical_json(&self.content()).as_bytes())
    }
}

impl Store {
    pub fn init(path: &Path) -> anyhow::Result<Self> {
        let mut connection = Connection::open(path).context("opening hoja database")?;
        set_mode(path, 0o600)?;
        connection.busy_timeout(std::time::Duration::from_secs(5))?;
        connection.pragma_update(None, "foreign_keys", "ON")?;
        connection.pragma_update(None, "journal_mode", "WAL")?;
        connection.pragma_update(None, "synchronous", "NORMAL")?;
        migrate(&mut connection)?;
        // SQLite creates sidecars lazily. Restrict existing sidecars whenever the store opens.
        for suffix in ["-wal", "-shm"] {
            let sidecar = PathBuf::from(format!("{}{}", path.display(), suffix));
            if sidecar.exists() {
                set_mode(&sidecar, 0o600)?;
            }
        }
        let mut types = HashMap::new();
        {
            let mut statement =
                connection.prepare("SELECT name, accepted_body FROM type_configs")?;
            let rows = statement
                .query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))?;
            for row in rows {
                let (name, body) = row?;
                let config = TypeConfig::parse(&body)
                    .with_context(|| format!("accepted type `{name}` no longer parses"))?;
                types.insert(name, Arc::new(config));
            }
        }
        let mut accepted_templates = std::collections::BTreeMap::new();
        {
            let mut statement =
                connection.prepare("SELECT path, accepted_body FROM template_files")?;
            let rows = statement.query_map([], |r| {
                Ok((r.get::<_, String>(0)?, r.get::<_, Vec<u8>>(1)?))
            })?;
            for row in rows {
                let (path, body) = row?;
                accepted_templates.insert(path, TemplateFile::new(body));
            }
        }
        Ok(Self {
            connection: Mutex::new(connection),
            types: RwLock::new(types),
            templates: RwLock::new(TemplateSet::new(accepted_templates)),
        })
    }

    fn lock(&self) -> Result<MutexGuard<'_, Connection>, StoreError> {
        self.connection
            .lock()
            .map_err(|_| StoreError::Internal("database lock poisoned".into()))
    }

    pub fn config(&self, name: &str) -> Result<Arc<TypeConfig>, StoreError> {
        self.types
            .read()
            .map_err(|_| StoreError::Internal("type lock poisoned".into()))?
            .get(name)
            .cloned()
            .ok_or_else(|| StoreError::Invalid(format!("type `{name}` is not accepted")))
    }

    pub fn accepted_types(&self) -> Result<Vec<(String, Arc<TypeConfig>)>, StoreError> {
        let types = self
            .types
            .read()
            .map_err(|_| StoreError::Internal("type lock poisoned".into()))?;
        let mut list: Vec<_> = types.iter().map(|(k, v)| (k.clone(), v.clone())).collect();
        list.sort_by(|a, b| a.0.cmp(&b.0));
        Ok(list)
    }

    // ---- type files -------------------------------------------------------

    pub fn type_statuses(&self, files: &[TypeFile]) -> Result<Vec<TypeStatus>, StoreError> {
        let connection = self.lock()?;
        let accepted = accepted_rows(&connection)?;
        let types = self.accepted_types()?;
        let mut names: Vec<String> = files.iter().map(|f| f.name.clone()).collect();
        names.extend(accepted.keys().cloned());
        names.sort();
        names.dedup();
        Ok(names
            .into_iter()
            .map(|name| {
                let file = files.iter().find(|f| f.name == name);
                let row = accepted.get(&name);
                let state = match (file, row) {
                    (Some(f), Some(r)) if f.hash == r.0 => "accepted",
                    (Some(_), Some(_)) => "changed",
                    (Some(_), None) => "new",
                    (None, _) => "missing",
                };
                let error = file
                    .filter(|_| state != "accepted")
                    .and_then(|f| TypeConfig::parse(&f.body).err().map(|e| format!("{e:#}")));
                TypeStatus {
                    state: state.into(),
                    file_hash: file.map(|f| f.hash.clone()),
                    accepted_hash: row.map(|r| r.0.clone()),
                    accepted_at: row.map(|r| r.2.clone()),
                    error,
                    config: types
                        .iter()
                        .find(|(n, _)| *n == name)
                        .map(|(_, c)| (**c).clone()),
                    name,
                }
            })
            .collect())
    }

    pub fn type_change(&self, name: &str, files: &[TypeFile]) -> Result<TypeChange, StoreError> {
        let connection = self.lock()?;
        let accepted = accepted_rows(&connection)?;
        let file = files.iter().find(|f| f.name == name);
        let row = accepted.get(name);
        if file.is_none() && row.is_none() {
            return Err(StoreError::NotFound);
        }
        Ok(TypeChange {
            name: name.into(),
            file_hash: file.map(|f| f.hash.clone()),
            file_body: file.map(|f| f.body.clone()),
            accepted_body: row.map(|r| r.1.clone()),
            error: file.and_then(|f| TypeConfig::parse(&f.body).err().map(|e| format!("{e:#}"))),
        })
    }

    /// Accepts the type file whose contents hash to `hash`, so the person
    /// accepts exactly the text they read.
    pub fn accept_type(
        &self,
        actor: &Actor,
        name: &str,
        hash: &str,
        files: &[TypeFile],
    ) -> Result<(), StoreError> {
        gates::person_only_action(actor, "accept a type file").map_err(StoreError::Refused)?;
        let file = files
            .iter()
            .find(|f| f.name == name)
            .ok_or(StoreError::NotFound)?;
        if file.hash != hash {
            return Err(StoreError::Conflict);
        }
        let config = TypeConfig::parse(&file.body)
            .map_err(|e| StoreError::Invalid(format!("type `{name}` is invalid: {e:#}")))?;
        let connection = self.lock()?;
        connection
            .execute(
                "INSERT INTO type_configs(name, accepted_hash, accepted_body, accepted_at) VALUES(?1,?2,?3,?4) ON CONFLICT(name) DO UPDATE SET accepted_hash=excluded.accepted_hash, accepted_body=excluded.accepted_body, accepted_at=excluded.accepted_at",
                params![name, file.hash, file.body, now_utc()],
            )
            .map_err(internal)?;
        self.types
            .write()
            .map_err(|_| StoreError::Internal("type lock poisoned".into()))?
            .insert(name.into(), Arc::new(config));
        Ok(())
    }

    // ---- template files ---------------------------------------------------

    pub fn template_set(&self) -> Result<TemplateSet, StoreError> {
        Ok(self
            .templates
            .read()
            .map_err(|_| StoreError::Internal("template lock poisoned".into()))?
            .clone())
    }

    /// The accepted files that render documents of this type.
    pub fn template_closure(
        &self,
        config: &TypeConfig,
    ) -> Result<(String, std::collections::BTreeMap<String, TemplateFile>), StoreError> {
        let entry = config.template.as_deref().ok_or_else(|| {
            StoreError::Invalid("this type has no template, so it has no PDF".into())
        })?;
        let files = self
            .template_set()?
            .closure(entry)
            .map_err(StoreError::Invalid)?;
        Ok((entry.to_string(), files))
    }

    /// Types whose template reaches `path` once the file on disk is accepted.
    fn template_users(
        &self,
        path: &str,
        disk: Option<&DiskTemplate>,
    ) -> Result<Vec<String>, StoreError> {
        let set = match disk {
            Some(d) => self.template_set()?.with(path, d.file.clone()),
            None => self.template_set()?,
        };
        Ok(self
            .accepted_types()?
            .into_iter()
            .filter(|(_, c)| {
                c.template
                    .as_deref()
                    .is_some_and(|entry| templates::reaches(&set, entry, path))
            })
            .map(|(name, _)| name)
            .collect())
    }

    pub fn template_statuses(
        &self,
        files: &[DiskTemplate],
    ) -> Result<Vec<TemplateStatus>, StoreError> {
        let accepted = accepted_template_rows(&*self.lock()?)?;
        let mut paths: Vec<String> = files.iter().map(|f| f.path.clone()).collect();
        paths.extend(accepted.keys().cloned());
        paths.sort();
        paths.dedup();
        paths
            .into_iter()
            .map(|path| {
                let file = files.iter().find(|f| f.path == path);
                let row = accepted.get(&path);
                let state = match (file, row) {
                    (Some(f), Some(r)) if f.file.hash == r.0 => "accepted",
                    (Some(_), Some(_)) => "changed",
                    (Some(_), None) => "new",
                    (None, _) => "missing",
                };
                Ok(TemplateStatus {
                    used_by: self.template_users(&path, file)?,
                    state: state.into(),
                    file_hash: file.map(|f| f.file.hash.clone()),
                    accepted_hash: row.map(|r| r.0.clone()),
                    accepted_at: row.map(|r| r.1.clone()),
                    error: file.and_then(|f| f.error.clone()),
                    path,
                })
            })
            .collect()
    }

    pub fn template_change(
        &self,
        path: &str,
        files: &[DiskTemplate],
    ) -> Result<TemplateChange, StoreError> {
        let file = files.iter().find(|f| f.path == path);
        let accepted = self.template_set()?;
        let accepted = accepted
            .get(path)
            .filter(|_| path != templates::BUILTIN_PATH);
        if file.is_none() && accepted.is_none() {
            return Err(StoreError::NotFound);
        }
        let text = |f: &TemplateFile| String::from_utf8(f.body.to_vec()).ok();
        Ok(TemplateChange {
            path: path.into(),
            file_hash: file.map(|f| f.file.hash.clone()),
            file_text: file.and_then(|f| text(&f.file)),
            accepted_text: accepted.and_then(text),
            binary: file.is_some_and(|f| text(&f.file).is_none())
                || accepted.is_some_and(|f| text(f).is_none()),
            file_size: file.map(|f| f.file.body.len()),
            accepted_size: accepted.map(|f| f.body.len()),
            error: file.and_then(|f| f.error.clone()),
            used_by: self.template_users(path, file)?,
        })
    }

    /// Accepts the template file whose contents hash to `hash`.
    pub fn accept_template(
        &self,
        actor: &Actor,
        path: &str,
        hash: &str,
        files: &[DiskTemplate],
    ) -> Result<(), StoreError> {
        gates::person_only_action(actor, "accept a template").map_err(StoreError::Refused)?;
        let file = files
            .iter()
            .find(|f| f.path == path)
            .ok_or(StoreError::NotFound)?;
        if let Some(error) = &file.error {
            return Err(StoreError::Invalid(error.clone()));
        }
        if file.file.hash != hash {
            return Err(StoreError::Conflict);
        }
        let connection = self.lock()?;
        connection
            .execute(
                "INSERT INTO template_files(path, accepted_hash, accepted_body, accepted_at) VALUES(?1,?2,?3,?4) ON CONFLICT(path) DO UPDATE SET accepted_hash=excluded.accepted_hash, accepted_body=excluded.accepted_body, accepted_at=excluded.accepted_at",
                params![path, file.file.hash, file.file.body.as_slice(), now_utc()],
            )
            .map_err(internal)?;
        self.templates
            .write()
            .map_err(|_| StoreError::Internal("template lock poisoned".into()))?
            .insert(path, file.file.clone());
        Ok(())
    }

    // ---- records ----------------------------------------------------------

    pub fn records(
        &self,
        type_name: Option<&str>,
        kind: Option<&str>,
    ) -> Result<Vec<Record>, StoreError> {
        let connection = self.lock()?;
        let mut statement = connection
            .prepare(&format!(
                "{RECORD_SELECT} WHERE r.deleted_at IS NULL AND (?1 IS NULL OR r.type=?1) AND (?2 IS NULL OR r.kind=?2) ORDER BY r.created_at, r.id"
            ))
            .map_err(internal)?;
        statement
            .query_map(params![type_name, kind], record_from_row)
            .map_err(internal)?
            .collect::<rusqlite::Result<Vec<_>>>()
            .map_err(internal)
    }

    pub fn record(&self, id: &str) -> Result<Record, StoreError> {
        let connection = self.lock()?;
        load_record(&connection, id)?.ok_or(StoreError::NotFound)
    }

    pub fn revisions(&self, target_kind: &str, id: &str) -> Result<Vec<Revision>, StoreError> {
        revisions_in(&*self.lock()?, target_kind, id)
    }

    pub fn create_record(
        &self,
        actor: &Actor,
        input: CreateRecord,
    ) -> Result<Outcome<Record>, StoreError> {
        validate_note(&input.note)?;
        let config = self.config(&input.type_name)?;
        let kind = config
            .kind(&input.kind)
            .ok_or_else(|| StoreError::Invalid(format!("unknown kind `{}`", input.kind)))?;
        let names: Vec<String> = input.fields.keys().cloned().collect();
        let write = Write {
            op: Op::Create,
            kind: Some(&input.kind),
            settled: false,
            fields: &names,
        };
        let route = gates::route(actor, &config, write);
        let fields =
            apply_patch(&kind.fields, &Map::new(), &input.fields).map_err(StoreError::Invalid)?;
        let mut connection = self.lock()?;
        let tx = connection.transaction().map_err(internal)?;
        validate_refs(&tx, &input.type_name, &kind.fields, &fields)?;
        let outcome = match route {
            Route::Refused(reason) => return Err(StoreError::Refused(reason)),
            Route::Direct => Outcome::Applied {
                result: insert_record(
                    &tx,
                    &input.type_name,
                    &input.kind,
                    &fields,
                    Attrib::of(actor),
                )?,
            },
            Route::Card => Outcome::AwaitingPerson {
                approval_id: open_card(
                    &tx,
                    actor,
                    Action::RecordCreate,
                    &input.type_name,
                    None,
                    None,
                    json!({"kind": input.kind, "fields": fields}),
                    &input.note,
                )?,
            },
        };
        tx.commit().map_err(internal)?;
        Ok(outcome)
    }

    pub fn update_record(
        &self,
        actor: &Actor,
        id: &str,
        input: UpdateRecord,
    ) -> Result<Outcome<Record>, StoreError> {
        validate_note(&input.note)?;
        let mut connection = self.lock()?;
        let tx = connection.transaction().map_err(internal)?;
        let record = load_live_record(&tx, id)?;
        if record.version != input.version {
            return Err(StoreError::Conflict);
        }
        let config = self.config(&record.type_name)?;
        let kind = config.kind(&record.kind).ok_or_else(|| {
            StoreError::Invalid(format!("kind `{}` is no longer in its type", record.kind))
        })?;
        let names: Vec<String> = input.fields.keys().cloned().collect();
        let write = Write {
            op: Op::Update,
            kind: Some(&record.kind),
            settled: record.reviewed,
            fields: &names,
        };
        let route = gates::route(actor, &config, write);
        if let Route::Refused(reason) = route {
            return Err(StoreError::Refused(reason));
        }
        let next = apply_patch(&kind.fields, &record.fields, &input.fields)
            .map_err(StoreError::Invalid)?;
        validate_refs(&tx, &record.type_name, &kind.fields, &next)?;
        if next == record.fields {
            return Ok(Outcome::Applied { result: record });
        }
        let outcome = match route {
            Route::Card => {
                let patch: Map<String, Value> = names
                    .iter()
                    .map(|n| (n.clone(), next.get(n).cloned().unwrap_or(Value::Null)))
                    .collect();
                Outcome::AwaitingPerson {
                    approval_id: open_card(
                        &tx,
                        actor,
                        Action::RecordUpdate,
                        &record.type_name,
                        Some(id),
                        Some(record.version),
                        json!({"fields": patch}),
                        &input.note,
                    )?,
                }
            }
            _ => Outcome::Applied {
                result: write_record_fields(&tx, &record, &next, Attrib::of(actor))?,
            },
        };
        tx.commit().map_err(internal)?;
        Ok(outcome)
    }

    pub fn set_reviewed(
        &self,
        actor: &Actor,
        id: &str,
        version: i64,
        reviewed: bool,
    ) -> Result<Record, StoreError> {
        gates::person_only_action(actor, "mark content reviewed").map_err(StoreError::Refused)?;
        let mut connection = self.lock()?;
        let tx = connection.transaction().map_err(internal)?;
        let record = load_live_record(&tx, id)?;
        if record.version != version {
            return Err(StoreError::Conflict);
        }
        if record.reviewed == reviewed {
            return Ok(record);
        }
        let now = now_utc();
        tx.execute(
            "UPDATE records SET reviewed=?2, reviewed_at=?3, version=version+1 WHERE id=?1",
            params![id, reviewed, reviewed.then_some(&now)],
        )
        .map_err(internal)?;
        let record = load_live_record(&tx, id)?;
        let change = if reviewed { "reviewed" } else { "unreviewed" };
        write_revision(
            &tx,
            "record",
            id,
            record.version,
            Attrib::of(actor),
            change,
            &record_snapshot(&record),
        )?;
        tx.commit().map_err(internal)?;
        Ok(record)
    }

    pub fn delete_record(&self, actor: &Actor, id: &str, version: i64) -> Result<(), StoreError> {
        gates::person_only_action(actor, "delete records").map_err(StoreError::Refused)?;
        let mut connection = self.lock()?;
        let tx = connection.transaction().map_err(internal)?;
        let record = load_live_record(&tx, id)?;
        if record.version != version {
            return Err(StoreError::Conflict);
        }
        let now = now_utc();
        tx.execute(
            "UPDATE records SET deleted_at=?2, updated_at=?2, version=version+1 WHERE id=?1",
            params![id, now],
        )
        .map_err(internal)?;
        write_revision(
            &tx,
            "record",
            id,
            record.version + 1,
            Attrib::of(actor),
            "delete",
            &record_snapshot(&record),
        )?;
        tx.commit().map_err(internal)?;
        Ok(())
    }

    // ---- documents --------------------------------------------------------

    pub fn documents(&self, type_name: Option<&str>) -> Result<Vec<Document>, StoreError> {
        let connection = self.lock()?;
        let ids: Vec<String> = {
            let mut statement = connection
                .prepare("SELECT id FROM documents WHERE (?1 IS NULL OR type=?1) ORDER BY updated_at DESC, id")
                .map_err(internal)?;
            statement
                .query_map(params![type_name], |r| r.get(0))
                .map_err(internal)?
                .collect::<rusqlite::Result<Vec<_>>>()
                .map_err(internal)?
        };
        ids.iter()
            .map(|id| self.resolve_in(&connection, id).map(|r| r.document))
            .collect()
    }

    pub fn document(&self, id: &str) -> Result<Document, StoreError> {
        let connection = self.lock()?;
        Ok(self.resolve_in(&connection, id)?.document)
    }

    pub fn resolve(&self, id: &str) -> Result<Resolved, StoreError> {
        let connection = self.lock()?;
        self.resolve_in(&connection, id)
    }

    /// The document and where each word of it came from, read under one lock
    /// so the provenance describes exactly the content its hash covers.
    pub fn inspect(&self, id: &str) -> Result<(Resolved, Provenance), StoreError> {
        let connection = self.lock()?;
        let resolved = self.resolve_in(&connection, id)?;
        let document_history = revisions_in(&connection, "document", id)?;
        let mut record_history = HashMap::new();
        let records = resolved
            .sections
            .iter()
            .flat_map(|s| s.records.iter())
            .chain(resolved.refs.values());
        for record in records {
            if !record_history.contains_key(&record.id) {
                record_history.insert(
                    record.id.clone(),
                    revisions_in(&connection, "record", &record.id)?,
                );
            }
        }
        let approved: Option<Value> = connection
            .query_row(
                "SELECT approved_content FROM documents WHERE id=?1",
                params![id],
                |r| r.get::<_, Option<String>>(0),
            )
            .map_err(internal)?
            .and_then(|raw| serde_json::from_str(&raw).ok());
        let labels = self.ref_labels(&connection, &resolved, approved.as_ref())?;
        let provenance = provenance::build(provenance::Inputs {
            resolved: &resolved,
            document_history: &document_history,
            record_history: &record_history,
            approved: approved.as_ref(),
            labels: &labels,
        });
        Ok((resolved, provenance))
    }

    /// Labels for every record a ref field names, now or in the approved content.
    fn ref_labels(
        &self,
        connection: &Connection,
        resolved: &Resolved,
        approved: Option<&Value>,
    ) -> Result<HashMap<String, String>, StoreError> {
        let mut ids: Vec<String> = Vec::new();
        let mut collect = |fields: &indexmap::IndexMap<String, crate::config::FieldConfig>,
                           values: &Map<String, Value>| {
            for (name, field) in fields {
                if field.ty == FieldType::Ref
                    && let Some(Value::String(id)) = values.get(name)
                {
                    ids.push(id.clone());
                }
            }
        };
        collect(&resolved.config.document.fields, &resolved.document.fields);
        if let Some(fields) = approved.and_then(|a| a["fields"].as_object()) {
            collect(&resolved.config.document.fields, fields);
        }
        for record in resolved.sections.iter().flat_map(|s| s.records.iter()) {
            if let Some(kind) = resolved.kind_config(record) {
                collect(&kind.fields, &record.fields);
            }
        }
        for section in approved
            .and_then(|a| a["sections"].as_array())
            .into_iter()
            .flatten()
        {
            for item in section["items"].as_array().into_iter().flatten() {
                let Some(id) = item["id"].as_str() else {
                    continue;
                };
                let (Some(record), Some(fields)) = (
                    resolved
                        .sections
                        .iter()
                        .flat_map(|s| s.records.iter())
                        .find(|r| r.id == id),
                    item["fields"].as_object(),
                ) else {
                    continue;
                };
                if let Some(kind) = resolved.kind_config(record) {
                    collect(&kind.fields, fields);
                }
            }
        }
        let mut labels = HashMap::new();
        for id in ids {
            if labels.contains_key(&id) {
                continue;
            }
            let Some(record) = load_record(connection, &id)? else {
                continue;
            };
            let label = self
                .config(&record.type_name)
                .ok()
                .and_then(|c| {
                    c.kind(&record.kind)
                        .and_then(|k| k.label_field())
                        .and_then(|f| record.fields.get(f))
                        .and_then(Value::as_str)
                        .map(String::from)
                })
                .unwrap_or_else(|| format!("{} {}", record.kind, &id[..id.len().min(8)]));
            labels.insert(id, label);
        }
        Ok(labels)
    }

    fn resolve_in(&self, connection: &Connection, id: &str) -> Result<Resolved, StoreError> {
        let mut document = load_document(connection, id)?.ok_or(StoreError::NotFound)?;
        let config = self.config(&document.type_name)?;
        let approved_hash: Option<String> = connection
            .query_row(
                "SELECT approved_hash FROM documents WHERE id=?1",
                params![id],
                |r| r.get(0),
            )
            .map_err(internal)?;
        let mut sections = Vec::new();
        let mut missing = Vec::new();
        let mut refs = HashMap::new();
        for section in &config.document.sections {
            let ids = if section.includes_all() {
                let source = config.section_type(section, &document.type_name);
                matching_ids(connection, source, section)?
            } else {
                document
                    .sections
                    .iter()
                    .find(|s| s.name == section.name)
                    .map(|s| s.record_ids.clone())
                    .unwrap_or_default()
            };
            let mut records = Vec::new();
            for record_id in ids {
                match load_record(connection, &record_id)? {
                    Some(record) if !record.deleted => {
                        if let Some(group) = &section.group_by
                            && let Some(Value::String(target)) = record.fields.get(group)
                            && !refs.contains_key(target)
                            && let Some(target_record) = load_record(connection, target)?
                        {
                            refs.insert(target.clone(), target_record);
                        }
                        records.push(record);
                    }
                    _ => missing.push((section.name.clone(), record_id)),
                }
            }
            sections.push(ResolvedSection {
                name: section.name.clone(),
                records,
            });
        }
        let mut configs = HashMap::new();
        configs.insert(document.type_name.clone(), config.clone());
        for section in &config.document.sections {
            if let Some(other) = &section.from_type
                && !configs.contains_key(other)
                && let Ok(other_config) = self.config(other)
            {
                configs.insert(other.clone(), other_config);
            }
        }
        let template = self.template_set()?.fingerprint(config.template.as_deref());
        let mut resolved = Resolved {
            config,
            configs,
            document: document.clone(),
            sections,
            refs,
            missing,
            template,
        };
        document.approved = approved_hash.is_some_and(|h| h == resolved.content_hash());
        resolved.document = document;
        Ok(resolved)
    }

    pub fn create_document(
        &self,
        actor: &Actor,
        input: CreateDocument,
    ) -> Result<Outcome<Document>, StoreError> {
        validate_note(&input.note)?;
        let title = validate_title(&input.title)?;
        let config = self.config(&input.type_name)?;
        let names: Vec<String> = input.fields.keys().cloned().collect();
        let write = Write {
            op: Op::Create,
            kind: None,
            settled: false,
            fields: &names,
        };
        let route = gates::route(actor, &config, write);
        let fields = apply_patch(&config.document.fields, &Map::new(), &input.fields)
            .map_err(StoreError::Invalid)?;
        let mut connection = self.lock()?;
        let tx = connection.transaction().map_err(internal)?;
        let outcome = match route {
            Route::Refused(reason) => return Err(StoreError::Refused(reason)),
            Route::Direct => {
                let id =
                    insert_document(&tx, &input.type_name, &title, &fields, Attrib::of(actor))?;
                Outcome::Applied {
                    result: self.resolve_in(&tx, &id)?.document,
                }
            }
            Route::Card => Outcome::AwaitingPerson {
                approval_id: open_card(
                    &tx,
                    actor,
                    Action::DocumentCreate,
                    &input.type_name,
                    None,
                    None,
                    json!({"title": title, "fields": fields}),
                    &input.note,
                )?,
            },
        };
        tx.commit().map_err(internal)?;
        Ok(outcome)
    }

    pub fn update_document(
        &self,
        actor: &Actor,
        id: &str,
        input: UpdateDocument,
    ) -> Result<Outcome<Document>, StoreError> {
        validate_note(&input.note)?;
        let mut connection = self.lock()?;
        let tx = connection.transaction().map_err(internal)?;
        let document = load_document(&tx, id)?.ok_or(StoreError::NotFound)?;
        if document.version != input.version {
            return Err(StoreError::Conflict);
        }
        let config = self.config(&document.type_name)?;
        let names: Vec<String> = input.fields.keys().cloned().collect();
        let write = Write {
            op: Op::Update,
            kind: None,
            settled: document.approved_at.is_some(),
            fields: &names,
        };
        let route = gates::route(actor, &config, write);
        if let Route::Refused(reason) = route {
            return Err(StoreError::Refused(reason));
        }
        let title = match &input.title {
            Some(title) => validate_title(title)?,
            None => document.title.clone(),
        };
        let fields = apply_patch(&config.document.fields, &document.fields, &input.fields)
            .map_err(StoreError::Invalid)?;
        if title == document.title && fields == document.fields {
            return Ok(Outcome::Applied {
                result: self.resolve_in(&tx, id)?.document,
            });
        }
        let outcome = match route {
            Route::Card => {
                let patch: Map<String, Value> = names
                    .iter()
                    .map(|n| (n.clone(), fields.get(n).cloned().unwrap_or(Value::Null)))
                    .collect();
                let mut payload = json!({"fields": patch});
                if title != document.title {
                    payload["title"] = Value::String(title);
                }
                Outcome::AwaitingPerson {
                    approval_id: open_card(
                        &tx,
                        actor,
                        Action::DocumentUpdate,
                        &document.type_name,
                        Some(id),
                        Some(document.version),
                        payload,
                        &input.note,
                    )?,
                }
            }
            _ => {
                write_document_fields(&tx, id, &title, &fields, Attrib::of(actor))?;
                Outcome::Applied {
                    result: self.resolve_in(&tx, id)?.document,
                }
            }
        };
        tx.commit().map_err(internal)?;
        Ok(outcome)
    }

    pub fn select(
        &self,
        actor: &Actor,
        id: &str,
        section: &str,
        input: SelectSection,
    ) -> Result<Outcome<Document>, StoreError> {
        validate_note(&input.note)?;
        let mut connection = self.lock()?;
        let tx = connection.transaction().map_err(internal)?;
        let document = load_document(&tx, id)?.ok_or(StoreError::NotFound)?;
        if document.version != input.version {
            return Err(StoreError::Conflict);
        }
        let config = self.config(&document.type_name)?;
        let write = Write {
            op: Op::Select,
            kind: None,
            settled: document.approved_at.is_some(),
            fields: &[],
        };
        let route = gates::route(actor, &config, write);
        if let Route::Refused(reason) = route {
            return Err(StoreError::Refused(reason));
        }
        validate_selection(
            &tx,
            &config,
            &document.type_name,
            section,
            &input.record_ids,
            &|name| self.config(name),
        )?;
        let current = section_ids(&document, section);
        if current == input.record_ids {
            return Ok(Outcome::Applied {
                result: self.resolve_in(&tx, id)?.document,
            });
        }
        let outcome = match route {
            Route::Card => Outcome::AwaitingPerson {
                approval_id: open_card(
                    &tx,
                    actor,
                    Action::DocumentSelect,
                    &document.type_name,
                    Some(id),
                    Some(document.version),
                    json!({"section": section, "record_ids": input.record_ids}),
                    &input.note,
                )?,
            },
            _ => {
                write_section(&tx, id, section, &input.record_ids, Attrib::of(actor))?;
                Outcome::Applied {
                    result: self.resolve_in(&tx, id)?.document,
                }
            }
        };
        tx.commit().map_err(internal)?;
        Ok(outcome)
    }

    /// Records the person's approval of the document's current content.
    /// `hash` is the content hash the person was shown: if anything changed
    /// since, including a selected record or the template, approval is
    /// refused. Lints are checked against the same content in the same
    /// transaction.
    pub fn approve_document(
        &self,
        actor: &Actor,
        id: &str,
        version: i64,
        hash: &str,
    ) -> Result<Document, StoreError> {
        gates::person_only_action(actor, "approve documents").map_err(StoreError::Refused)?;
        let mut connection = self.lock()?;
        let tx = connection.transaction().map_err(internal)?;
        let resolved = self.resolve_in(&tx, id)?;
        if resolved.document.version != version || resolved.content_hash() != hash {
            return Err(StoreError::Conflict);
        }
        if !resolved.missing.is_empty() {
            return Err(StoreError::Invalid(
                "remove deleted records from the selection first".into(),
            ));
        }
        let blocking = lint::errors(&lint::lint(&resolved));
        if blocking > 0 {
            return Err(StoreError::Invalid(format!(
                "fix {blocking} lint error{} before approving",
                if blocking == 1 { "" } else { "s" }
            )));
        }
        tx.execute(
            "UPDATE documents SET approved_hash=?2, approved_content=?3, approved_at=?4, version=version+1 WHERE id=?1",
            params![id, hash, canonical_json(&resolved.content()), now_utc()],
        )
        .map_err(internal)?;
        let document = self.resolve_in(&tx, id)?.document;
        write_revision(
            &tx,
            "document",
            id,
            document.version,
            Attrib::of(actor),
            "approve",
            &document_snapshot(&document),
        )?;
        tx.commit().map_err(internal)?;
        Ok(document)
    }

    // ---- cards ------------------------------------------------------------

    pub fn approvals(&self, state: Option<&str>) -> Result<Vec<Approval>, StoreError> {
        let connection = self.lock()?;
        let mut statement = connection
            .prepare(&format!(
                "{APPROVAL_SELECT} WHERE (?1 IS NULL OR state=?1) ORDER BY created_at, id"
            ))
            .map_err(internal)?;
        let mut approvals = statement
            .query_map(params![state], approval_from_row)
            .map_err(internal)?
            .collect::<rusqlite::Result<Vec<_>>>()
            .map_err(internal)?;
        for approval in &mut approvals {
            hydrate_approval(&connection, approval)?;
        }
        Ok(approvals)
    }

    /// Record that card `card_id` was raised in Tracon as `approval_id`
    /// (`crate::node_cards`). A card is raised once.
    pub fn link_node_card(&self, card_id: &str, approval_id: &str) -> Result<(), StoreError> {
        let connection = self.lock()?;
        connection
            .execute(
                "INSERT INTO node_cards(card_id, approval_id, raised_at) VALUES (?1, ?2, ?3) \
                 ON CONFLICT(card_id) DO NOTHING",
                params![card_id, approval_id, now_utc()],
            )
            .map_err(internal)?;
        Ok(())
    }

    /// The Tracon approval card `card_id` was raised as, if it was.
    pub fn node_card(&self, card_id: &str) -> Result<Option<String>, StoreError> {
        let connection = self.lock()?;
        connection
            .query_row(
                "SELECT approval_id FROM node_cards WHERE card_id=?1",
                params![card_id],
                |r| r.get(0),
            )
            .optional()
            .map_err(internal)
    }

    /// `(card, Tracon approval)` for each card raised in Tracon that is
    /// still pending here, oldest first.
    pub fn node_cards_waiting(&self) -> Result<Vec<(String, String)>, StoreError> {
        let connection = self.lock()?;
        let mut statement = connection
            .prepare(
                "SELECT n.card_id, n.approval_id FROM node_cards n JOIN approvals a ON a.id=n.card_id \
                 WHERE a.state='pending' ORDER BY n.raised_at, n.card_id",
            )
            .map_err(internal)?;
        statement
            .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))
            .map_err(internal)?
            .collect::<rusqlite::Result<Vec<_>>>()
            .map_err(internal)
    }

    pub fn approval(&self, id: &str) -> Result<Approval, StoreError> {
        let connection = self.lock()?;
        let mut approval = load_approval(&connection, id)?.ok_or(StoreError::NotFound)?;
        hydrate_approval(&connection, &mut approval)?;
        Ok(approval)
    }

    pub fn decide(&self, actor: &Actor, id: &str, input: Decide) -> Result<Approval, StoreError> {
        gates::person_only_action(actor, "decide cards").map_err(StoreError::Refused)?;
        let reason = input
            .reason
            .as_deref()
            .map(str::trim)
            .filter(|r| !r.is_empty());
        if let Some(reason) = reason {
            validate_note(reason)?;
        }
        if input.decision != Decision::Accept && input.payload.is_some() {
            return Err(StoreError::Invalid(
                "only an accepted card takes an edited payload".into(),
            ));
        }
        let mut connection = self.lock()?;
        let tx = connection.transaction().map_err(internal)?;
        let approval = load_approval(&tx, id)?.ok_or(StoreError::NotFound)?;
        if approval.state != "pending" {
            return Err(StoreError::Conflict);
        }
        let now = now_utc();
        match input.decision {
            Decision::Reject | Decision::RequestChanges => {
                let state = if input.decision == Decision::Reject {
                    "rejected"
                } else {
                    "changes_requested"
                };
                tx.execute(
                    "UPDATE approvals SET state=?2, reason=?3, decided_at=?4 WHERE id=?1",
                    params![id, state, reason, now],
                )
                .map_err(internal)?;
            }
            Decision::Accept => {
                let payload = match input.payload {
                    Some(edit) => {
                        check_edit(&approval, &edit)?;
                        edit
                    }
                    None => approval.payload.clone(),
                };
                let edited = payload != approval.payload;
                if is_stale(&tx, &approval)? {
                    return Err(StoreError::Stale);
                }
                let result = self.apply_card(&tx, &approval, &payload, edited)?;
                tx.execute(
                    "UPDATE approvals SET state='accepted', reason=?2, edited=?3, result_json=?4, decided_at=?5 WHERE id=?1",
                    params![id, reason, edited, result.to_string(), now],
                )
                .map_err(internal)?;
            }
        }
        let mut approval = load_approval(&tx, id)?.ok_or(StoreError::NotFound)?;
        hydrate_approval(&tx, &mut approval)?;
        tx.commit().map_err(internal)?;
        Ok(approval)
    }

    /// Applies an accepted card's payload, re-checking it against the type as
    /// it is now, since the person may have accepted a new type file meanwhile.
    fn apply_card(
        &self,
        tx: &Connection,
        approval: &Approval,
        payload: &Value,
        edited: bool,
    ) -> Result<Value, StoreError> {
        let config = self.config(&approval.type_name)?;
        let agent = Actor::Agent(approval.agent.clone());
        let attrib = Attrib {
            author: Author::AgentAccepted,
            agent: Some(&approval.agent),
            approval_id: Some(&approval.id),
            edited,
        };
        let fields_of = |payload: &Value| -> Map<String, Value> {
            payload
                .get("fields")
                .and_then(Value::as_object)
                .cloned()
                .unwrap_or_default()
        };
        let recheck = |write: Write<'_>| -> Result<(), StoreError> {
            match gates::route(&agent, &config, write) {
                Route::Refused(reason) => Err(StoreError::Refused(format!(
                    "this card can no longer be accepted: {reason}"
                ))),
                _ => Ok(()),
            }
        };
        let target = || {
            approval
                .target_id
                .as_deref()
                .ok_or(StoreError::Internal("card without a target".into()))
        };
        match approval.action {
            Action::RecordCreate => {
                let kind_name = payload["kind"].as_str().unwrap_or_default();
                let kind = config.kind(kind_name).ok_or_else(|| {
                    StoreError::Invalid(format!("kind `{kind_name}` is no longer in its type"))
                })?;
                let patch = fields_of(payload);
                let names: Vec<String> = patch.keys().cloned().collect();
                // A person's acceptance stands in for the agent, so only the
                // gates on what an agent may propose apply.
                recheck(Write {
                    op: Op::Create,
                    kind: Some(kind_name),
                    settled: false,
                    fields: &names,
                })?;
                let fields =
                    apply_patch(&kind.fields, &Map::new(), &patch).map_err(StoreError::Invalid)?;
                validate_refs(tx, &approval.type_name, &kind.fields, &fields)?;
                let record = insert_record(tx, &approval.type_name, kind_name, &fields, attrib)?;
                Ok(json!({"record_id": record.id, "version": record.version}))
            }
            Action::RecordUpdate => {
                let record = load_live_record(tx, target()?)?;
                let kind = config.kind(&record.kind).ok_or_else(|| {
                    StoreError::Invalid(format!("kind `{}` is no longer in its type", record.kind))
                })?;
                let patch = fields_of(payload);
                let names: Vec<String> = patch.keys().cloned().collect();
                recheck(Write {
                    op: Op::Update,
                    kind: Some(&record.kind),
                    settled: false,
                    fields: &names,
                })?;
                let next = apply_patch(&kind.fields, &record.fields, &patch)
                    .map_err(StoreError::Invalid)?;
                validate_refs(tx, &record.type_name, &kind.fields, &next)?;
                let record = write_record_fields(tx, &record, &next, attrib)?;
                Ok(json!({"record_id": record.id, "version": record.version}))
            }
            Action::DocumentCreate => {
                let patch = fields_of(payload);
                let names: Vec<String> = patch.keys().cloned().collect();
                recheck(Write {
                    op: Op::Create,
                    kind: None,
                    settled: false,
                    fields: &names,
                })?;
                let title = validate_title(payload["title"].as_str().unwrap_or_default())?;
                let fields = apply_patch(&config.document.fields, &Map::new(), &patch)
                    .map_err(StoreError::Invalid)?;
                let id = insert_document(tx, &approval.type_name, &title, &fields, attrib)?;
                Ok(json!({"document_id": id, "version": 1}))
            }
            Action::DocumentUpdate => {
                let id = target()?;
                let document = load_document(tx, id)?.ok_or(StoreError::NotFound)?;
                let patch = fields_of(payload);
                let names: Vec<String> = patch.keys().cloned().collect();
                recheck(Write {
                    op: Op::Update,
                    kind: None,
                    settled: false,
                    fields: &names,
                })?;
                let title = match payload.get("title").and_then(Value::as_str) {
                    Some(title) => validate_title(title)?,
                    None => document.title.clone(),
                };
                let fields = apply_patch(&config.document.fields, &document.fields, &patch)
                    .map_err(StoreError::Invalid)?;
                let version = write_document_fields(tx, id, &title, &fields, attrib)?;
                Ok(json!({"document_id": id, "version": version}))
            }
            Action::DocumentSelect => {
                let id = target()?;
                let section = payload["section"].as_str().unwrap_or_default();
                let ids = string_list(&payload["record_ids"])?;
                load_document(tx, id)?.ok_or(StoreError::NotFound)?;
                validate_selection(tx, &config, &approval.type_name, section, &ids, &|name| {
                    self.config(name)
                })?;
                let version = write_section(tx, id, section, &ids, attrib)?;
                Ok(json!({"document_id": id, "version": version}))
            }
        }
    }
}

// ---- row access -----------------------------------------------------------

fn revisions_in(
    connection: &Connection,
    target_kind: &str,
    id: &str,
) -> Result<Vec<Revision>, StoreError> {
    let mut statement = connection
        .prepare("SELECT version, author, agent, approval_id, edited, change, snapshot_json, created_at FROM revisions WHERE target_kind=?1 AND target_id=?2 ORDER BY version DESC")
        .map_err(internal)?;
    statement
        .query_map(params![target_kind, id], |r| {
            Ok(Revision {
                version: r.get(0)?,
                author: parse_author(&r.get::<_, String>(1)?),
                agent: r.get(2)?,
                approval_id: r.get(3)?,
                edited: r.get(4)?,
                change: r.get(5)?,
                snapshot: serde_json::from_str(&r.get::<_, String>(6)?).unwrap_or(Value::Null),
                created_at: r.get(7)?,
            })
        })
        .map_err(internal)?
        .collect::<rusqlite::Result<Vec<_>>>()
        .map_err(internal)
}

const RECORD_SELECT: &str = "SELECT r.id, r.type, r.kind, r.fields_json, r.reviewed, r.reviewed_at, r.version, r.created_at, r.updated_at, r.deleted_at,
 (SELECT author||char(31)||IFNULL(agent,'') FROM revisions v WHERE v.target_kind='record' AND v.target_id=r.id AND v.change IN ('create','update') ORDER BY v.version ASC LIMIT 1),
 (SELECT author||char(31)||IFNULL(agent,'') FROM revisions v WHERE v.target_kind='record' AND v.target_id=r.id AND v.change IN ('create','update') ORDER BY v.version DESC LIMIT 1)
 FROM records r";

const APPROVAL_SELECT: &str = "SELECT id, action, type, target_id, base_version, payload_json, agent, note, state, reason, edited, result_json, created_at, decided_at FROM approvals";

fn parse_author(s: &str) -> Author {
    match s {
        "agent" => Author::Agent,
        "agent_accepted" => Author::AgentAccepted,
        _ => Author::Person,
    }
}

fn attribution(raw: Option<String>) -> Option<Attribution> {
    let raw = raw?;
    let (author, agent) = raw.split_once('\u{1f}').unwrap_or((&raw, ""));
    Some(Attribution {
        author: parse_author(author),
        agent: (!agent.is_empty()).then(|| agent.to_string()),
    })
}

fn json_object(raw: &str) -> Map<String, Value> {
    serde_json::from_str(raw).unwrap_or_default()
}

fn record_from_row(row: &Row<'_>) -> rusqlite::Result<Record> {
    Ok(Record {
        id: row.get(0)?,
        type_name: row.get(1)?,
        kind: row.get(2)?,
        fields: json_object(&row.get::<_, String>(3)?),
        reviewed: row.get(4)?,
        reviewed_at: row.get(5)?,
        version: row.get(6)?,
        created_at: row.get(7)?,
        updated_at: row.get(8)?,
        deleted: row.get::<_, Option<String>>(9)?.is_some(),
        created_by: attribution(row.get(10)?),
        updated_by: attribution(row.get(11)?),
    })
}

fn load_record(connection: &Connection, id: &str) -> Result<Option<Record>, StoreError> {
    connection
        .query_row(
            &format!("{RECORD_SELECT} WHERE r.id=?1"),
            params![id],
            record_from_row,
        )
        .optional()
        .map_err(internal)
}

fn load_live_record(connection: &Connection, id: &str) -> Result<Record, StoreError> {
    load_record(connection, id)?
        .filter(|r| !r.deleted)
        .ok_or(StoreError::NotFound)
}

fn load_document(connection: &Connection, id: &str) -> Result<Option<Document>, StoreError> {
    let document = connection
        .query_row(
            "SELECT d.id, d.type, d.title, d.fields_json, d.approved_at, d.version, d.created_at, d.updated_at,
             (SELECT author||char(31)||IFNULL(agent,'') FROM revisions v WHERE v.target_kind='document' AND v.target_id=d.id AND v.change<>'approve' ORDER BY v.version ASC LIMIT 1),
             (SELECT author||char(31)||IFNULL(agent,'') FROM revisions v WHERE v.target_kind='document' AND v.target_id=d.id AND v.change<>'approve' ORDER BY v.version DESC LIMIT 1)
             FROM documents d WHERE d.id=?1",
            params![id],
            |r| {
                Ok(Document {
                    id: r.get(0)?,
                    type_name: r.get(1)?,
                    title: r.get(2)?,
                    fields: json_object(&r.get::<_, String>(3)?),
                    sections: Vec::new(),
                    approved: false,
                    approved_at: r.get(4)?,
                    version: r.get(5)?,
                    created_at: r.get(6)?,
                    updated_at: r.get(7)?,
                    created_by: attribution(r.get(8)?),
                    updated_by: attribution(r.get(9)?),
                })
            },
        )
        .optional()
        .map_err(internal)?;
    let Some(mut document) = document else {
        return Ok(None);
    };
    let mut statement = connection
        .prepare("SELECT section, record_id FROM document_items WHERE document_id=?1 ORDER BY section, position")
        .map_err(internal)?;
    let rows = statement
        .query_map(params![id], |r| {
            Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?))
        })
        .map_err(internal)?;
    for row in rows {
        let (section, record_id) = row.map_err(internal)?;
        match document.sections.iter_mut().find(|s| s.name == section) {
            Some(s) => s.record_ids.push(record_id),
            None => document.sections.push(Section {
                name: section,
                record_ids: vec![record_id],
            }),
        }
    }
    Ok(Some(document))
}

fn section_ids(document: &Document, section: &str) -> Vec<String> {
    document
        .sections
        .iter()
        .find(|s| s.name == section)
        .map(|s| s.record_ids.clone())
        .unwrap_or_default()
}

fn approval_from_row(row: &Row<'_>) -> rusqlite::Result<Approval> {
    let action: String = row.get(1)?;
    Ok(Approval {
        id: row.get(0)?,
        action: Action::parse(&action).unwrap_or(Action::RecordUpdate),
        type_name: row.get(2)?,
        target_id: row.get(3)?,
        base_version: row.get(4)?,
        payload: serde_json::from_str(&row.get::<_, String>(5)?).unwrap_or(Value::Null),
        agent: row.get(6)?,
        note: row.get(7)?,
        state: row.get(8)?,
        reason: row.get(9)?,
        edited: row.get(10)?,
        result: row
            .get::<_, Option<String>>(11)?
            .and_then(|s| serde_json::from_str(&s).ok()),
        created_at: row.get(12)?,
        decided_at: row.get(13)?,
        stale: false,
        current: Value::Null,
    })
}

fn load_approval(connection: &Connection, id: &str) -> Result<Option<Approval>, StoreError> {
    connection
        .query_row(
            &format!("{APPROVAL_SELECT} WHERE id=?1"),
            params![id],
            approval_from_row,
        )
        .optional()
        .map_err(internal)
}

fn hydrate_approval(connection: &Connection, approval: &mut Approval) -> Result<(), StoreError> {
    let Some(target) = approval.target_id.as_deref() else {
        return Ok(());
    };
    approval.current = match approval.action {
        Action::RecordUpdate => load_record(connection, target)?
            .map(|r| json!({"kind": r.kind, "fields": r.fields, "reviewed": r.reviewed, "version": r.version, "deleted": r.deleted}))
            .unwrap_or(Value::Null),
        Action::DocumentUpdate | Action::DocumentSelect => load_document(connection, target)?
            .map(|d| json!({"title": d.title, "fields": d.fields, "sections": d.sections, "version": d.version}))
            .unwrap_or(Value::Null),
        _ => Value::Null,
    };
    if approval.state == "pending" {
        approval.stale = is_stale(connection, approval)?;
    }
    Ok(())
}

fn base_snapshot(
    connection: &Connection,
    target_kind: &str,
    id: &str,
    version: i64,
) -> Result<Option<Value>, StoreError> {
    connection
        .query_row(
            "SELECT snapshot_json FROM revisions WHERE target_kind=?1 AND target_id=?2 AND version<=?3 ORDER BY version DESC LIMIT 1",
            params![target_kind, id, version],
            |r| r.get::<_, String>(0),
        )
        .optional()
        .map_err(internal)
        .map(|raw| raw.and_then(|s| serde_json::from_str(&s).ok()))
}

/// A card is stale when anything it touches changed after the version the agent read.
fn is_stale(connection: &Connection, approval: &Approval) -> Result<bool, StoreError> {
    let (Some(target), Some(base)) = (approval.target_id.as_deref(), approval.base_version) else {
        return Ok(false);
    };
    let touched: Vec<&String> = approval
        .payload
        .get("fields")
        .and_then(Value::as_object)
        .map(|m| m.keys().collect())
        .unwrap_or_default();
    match approval.action {
        Action::RecordUpdate => {
            let Some(record) = load_record(connection, target)?.filter(|r| !r.deleted) else {
                return Ok(true);
            };
            if record.version == base {
                return Ok(false);
            }
            let Some(snapshot) = base_snapshot(connection, "record", target, base)? else {
                return Ok(true);
            };
            Ok(touched.iter().any(|name| {
                snapshot["fields"].get(name.as_str()) != record.fields.get(name.as_str())
            }))
        }
        Action::DocumentUpdate | Action::DocumentSelect => {
            let Some(document) = load_document(connection, target)? else {
                return Ok(true);
            };
            if document.version == base {
                return Ok(false);
            }
            let Some(snapshot) = base_snapshot(connection, "document", target, base)? else {
                return Ok(true);
            };
            if approval.action == Action::DocumentSelect {
                let section = approval.payload["section"].as_str().unwrap_or_default();
                let before = snapshot["sections"]
                    .as_array()
                    .and_then(|list| list.iter().find(|s| s["name"] == section))
                    .map(|s| s["record_ids"].clone())
                    .unwrap_or(json!([]));
                return Ok(before != json!(section_ids(&document, section)));
            }
            let title_changed = approval.payload.get("title").is_some()
                && snapshot["title"] != json!(document.title);
            Ok(title_changed
                || touched.iter().any(|name| {
                    snapshot["fields"].get(name.as_str()) != document.fields.get(name.as_str())
                }))
        }
        _ => Ok(false),
    }
}

/// The person may change a card's values before accepting it, but not widen
/// it: an edit names the same target and no fields the agent didn't touch.
fn check_edit(approval: &Approval, edit: &Value) -> Result<(), StoreError> {
    let invalid = |m: &str| Err(StoreError::Invalid(m.into()));
    let Some(edit_obj) = edit.as_object() else {
        return invalid("the edited payload must be an object");
    };
    let original = approval.payload.as_object().cloned().unwrap_or_default();
    for key in edit_obj.keys() {
        if !original.contains_key(key) && key != "title" {
            return invalid("the edit adds a key the card did not have");
        }
    }
    match approval.action {
        Action::DocumentSelect => {
            if edit["section"] != original["section"] {
                return invalid("the edit may not change the section");
            }
            string_list(&edit["record_ids"])?;
        }
        Action::RecordCreate if edit["kind"] != original["kind"] => {
            return invalid("the edit may not change the kind");
        }
        _ => {}
    }
    if edit.get("title").is_some() && original.get("title").is_none() {
        return invalid("the edit adds a title the card did not change");
    }
    let original_fields = original.get("fields").and_then(Value::as_object);
    if let Some(fields) = edit.get("fields") {
        let Some(fields) = fields.as_object() else {
            return invalid("fields must be an object");
        };
        if fields
            .keys()
            .any(|k| original_fields.is_none_or(|o| !o.contains_key(k)))
        {
            return invalid("the edit touches a field the card did not");
        }
    }
    Ok(())
}

fn string_list(value: &Value) -> Result<Vec<String>, StoreError> {
    value
        .as_array()
        .and_then(|items| items.iter().map(|v| v.as_str().map(String::from)).collect())
        .ok_or_else(|| StoreError::Invalid("record_ids must be a list of ids".into()))
}

// ---- writes ---------------------------------------------------------------

fn record_snapshot(record: &Record) -> Value {
    json!({"kind": record.kind, "fields": record.fields, "reviewed": record.reviewed})
}

fn document_snapshot(document: &Document) -> Value {
    json!({"title": document.title, "fields": document.fields, "sections": document.sections, "approved": document.approved})
}

fn write_revision(
    connection: &Connection,
    target_kind: &str,
    id: &str,
    version: i64,
    attrib: Attrib<'_>,
    change: &str,
    snapshot: &Value,
) -> Result<(), StoreError> {
    connection
        .execute(
            "INSERT INTO revisions(target_kind, target_id, version, author, agent, approval_id, edited, change, snapshot_json, created_at) VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10)",
            params![
                target_kind,
                id,
                version,
                attrib.author.as_str(),
                attrib.agent,
                attrib.approval_id,
                attrib.edited,
                change,
                snapshot.to_string(),
                now_utc()
            ],
        )
        .map_err(internal)?;
    Ok(())
}

fn insert_record(
    connection: &Connection,
    type_name: &str,
    kind: &str,
    fields: &Map<String, Value>,
    attrib: Attrib<'_>,
) -> Result<Record, StoreError> {
    let id = Uuid::new_v4().to_string();
    let now = now_utc();
    connection
        .execute(
            "INSERT INTO records(id, type, kind, fields_json, created_at, updated_at) VALUES(?1,?2,?3,?4,?5,?5)",
            params![id, type_name, kind, Value::Object(fields.clone()).to_string(), now],
        )
        .map_err(internal)?;
    let record = load_live_record(connection, &id)?;
    write_revision(
        connection,
        "record",
        &id,
        1,
        attrib,
        "create",
        &record_snapshot(&record),
    )?;
    load_live_record(connection, &id)
}

/// Any content change clears `reviewed`: the person reviewed the old text.
fn write_record_fields(
    connection: &Connection,
    record: &Record,
    fields: &Map<String, Value>,
    attrib: Attrib<'_>,
) -> Result<Record, StoreError> {
    connection
        .execute(
            "UPDATE records SET fields_json=?2, reviewed=0, reviewed_at=NULL, version=version+1, updated_at=?3 WHERE id=?1",
            params![record.id, Value::Object(fields.clone()).to_string(), now_utc()],
        )
        .map_err(internal)?;
    let next = load_live_record(connection, &record.id)?;
    write_revision(
        connection,
        "record",
        &record.id,
        next.version,
        attrib,
        "update",
        &record_snapshot(&next),
    )?;
    load_live_record(connection, &record.id)
}

fn insert_document(
    connection: &Connection,
    type_name: &str,
    title: &str,
    fields: &Map<String, Value>,
    attrib: Attrib<'_>,
) -> Result<String, StoreError> {
    let id = Uuid::new_v4().to_string();
    let now = now_utc();
    connection
        .execute(
            "INSERT INTO documents(id, type, title, fields_json, created_at, updated_at) VALUES(?1,?2,?3,?4,?5,?5)",
            params![id, type_name, title, Value::Object(fields.clone()).to_string(), now],
        )
        .map_err(internal)?;
    let document = load_document(connection, &id)?.ok_or(StoreError::NotFound)?;
    write_revision(
        connection,
        "document",
        &id,
        1,
        attrib,
        "create",
        &document_snapshot(&document),
    )?;
    Ok(id)
}

fn write_document_fields(
    connection: &Connection,
    id: &str,
    title: &str,
    fields: &Map<String, Value>,
    attrib: Attrib<'_>,
) -> Result<i64, StoreError> {
    connection
        .execute(
            "UPDATE documents SET title=?2, fields_json=?3, version=version+1, updated_at=?4 WHERE id=?1",
            params![id, title, Value::Object(fields.clone()).to_string(), now_utc()],
        )
        .map_err(internal)?;
    let document = load_document(connection, id)?.ok_or(StoreError::NotFound)?;
    write_revision(
        connection,
        "document",
        id,
        document.version,
        attrib,
        "update",
        &document_snapshot(&document),
    )?;
    Ok(document.version)
}

fn write_section(
    connection: &Connection,
    id: &str,
    section: &str,
    record_ids: &[String],
    attrib: Attrib<'_>,
) -> Result<i64, StoreError> {
    connection
        .execute(
            "DELETE FROM document_items WHERE document_id=?1 AND section=?2",
            params![id, section],
        )
        .map_err(internal)?;
    for (position, record_id) in record_ids.iter().enumerate() {
        connection
            .execute(
                "INSERT INTO document_items(document_id, section, position, record_id) VALUES(?1,?2,?3,?4)",
                params![id, section, position as i64, record_id],
            )
            .map_err(internal)?;
    }
    connection
        .execute(
            "UPDATE documents SET version=version+1, updated_at=?2 WHERE id=?1",
            params![id, now_utc()],
        )
        .map_err(internal)?;
    let document = load_document(connection, id)?.ok_or(StoreError::NotFound)?;
    write_revision(
        connection,
        "document",
        id,
        document.version,
        attrib,
        &format!("select:{section}"),
        &document_snapshot(&document),
    )?;
    Ok(document.version)
}

#[allow(clippy::too_many_arguments)]
fn open_card(
    connection: &Connection,
    actor: &Actor,
    action: Action,
    type_name: &str,
    target_id: Option<&str>,
    base_version: Option<i64>,
    payload: Value,
    note: &str,
) -> Result<String, StoreError> {
    let Actor::Agent(agent) = actor else {
        return Err(StoreError::Internal("only agents open cards".into()));
    };
    let key = sha256_hex(
        json!([action.as_str(), type_name, target_id, base_version, payload])
            .to_string()
            .as_bytes(),
    );
    if let Some(existing) = connection
        .query_row(
            "SELECT id FROM approvals WHERE request_key=?1 AND state='pending'",
            params![key],
            |r| r.get::<_, String>(0),
        )
        .optional()
        .map_err(internal)?
    {
        return Ok(existing);
    }
    let id = Uuid::new_v4().to_string();
    connection
        .execute(
            "INSERT INTO approvals(id, request_key, action, type, target_id, base_version, payload_json, agent, note, state, created_at) VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,'pending',?10)",
            params![id, key, action.as_str(), type_name, target_id, base_version, payload.to_string(), agent, note.trim(), now_utc()],
        )
        .map_err(internal)?;
    Ok(id)
}

// ---- validation -----------------------------------------------------------

fn validate_note(note: &str) -> Result<(), StoreError> {
    if note.chars().count() > MAX_NOTE {
        return Err(StoreError::Invalid(format!(
            "notes are limited to {MAX_NOTE} characters"
        )));
    }
    Ok(())
}

fn validate_title(title: &str) -> Result<String, StoreError> {
    let title = title.trim();
    if title.is_empty() || title.chars().count() > 200 || title.contains('\n') {
        return Err(StoreError::Invalid(
            "a title is one line of 1 to 200 characters".into(),
        ));
    }
    Ok(title.to_string())
}

fn validate_refs(
    connection: &Connection,
    type_name: &str,
    declared: &indexmap::IndexMap<String, crate::config::FieldConfig>,
    fields: &Map<String, Value>,
) -> Result<(), StoreError> {
    for (name, field) in declared {
        if field.ty != FieldType::Ref {
            continue;
        }
        let Some(Value::String(target)) = fields.get(name) else {
            continue;
        };
        let record = load_record(connection, target)?.filter(|r| !r.deleted);
        let ok = record.is_some_and(|r| {
            r.type_name == type_name && Some(r.kind.as_str()) == field.ref_kind.as_deref()
        });
        if !ok {
            return Err(StoreError::Invalid(format!(
                "`{name}` must name a {} record",
                field.ref_kind.as_deref().unwrap_or("matching")
            )));
        }
    }
    Ok(())
}

fn validate_selection(
    connection: &Connection,
    config: &TypeConfig,
    type_name: &str,
    section: &str,
    ids: &[String],
    configs: &dyn Fn(&str) -> Result<Arc<TypeConfig>, StoreError>,
) -> Result<(), StoreError> {
    let section_config = config
        .section(section)
        .ok_or_else(|| StoreError::Invalid(format!("unknown section `{section}`")))?;
    if section_config.includes_all() {
        return Err(StoreError::Invalid(format!(
            "section `{section}` includes every matching record, so it isn't selected by hand"
        )));
    }
    if ids.len() > MAX_SELECTION {
        return Err(StoreError::Invalid(format!(
            "a section holds at most {MAX_SELECTION} records"
        )));
    }
    let source = config.section_type(section_config, type_name);
    if source != type_name {
        let other = configs(source)?;
        let kind = other.kind(&section_config.kind).ok_or_else(|| {
            StoreError::Invalid(format!(
                "section `{section}` selects `{}` records, which type `{source}` doesn't have",
                section_config.kind
            ))
        })?;
        if let Some(field) = section_config
            .filter
            .keys()
            .find(|f| !kind.fields.contains_key(*f))
        {
            return Err(StoreError::Invalid(format!(
                "section `{section}` filters on `{field}`, which `{source}` `{}` records don't have",
                section_config.kind
            )));
        }
    }
    for (i, id) in ids.iter().enumerate() {
        if ids[..i].contains(id) {
            return Err(StoreError::Invalid(format!(
                "record {id} is selected twice"
            )));
        }
        let record = load_record(connection, id)?
            .filter(|r| !r.deleted)
            .ok_or_else(|| StoreError::Invalid(format!("record {id} does not exist")))?;
        if record.type_name != source || record.kind != section_config.kind {
            return Err(StoreError::Invalid(format!(
                "section `{section}` takes {source} {} records",
                section_config.kind
            )));
        }
        for (field, expected) in &section_config.filter {
            if record.fields.get(field).and_then(Value::as_str) != Some(expected.as_str()) {
                return Err(StoreError::Invalid(format!(
                    "section `{section}` takes records whose {field} is {expected}"
                )));
            }
        }
    }
    Ok(())
}

/// The live records an `include = "all"` section holds: its type and kind,
/// matching its `where` filter, ordered by its `sort` field (records without
/// one last), then by creation.
fn matching_ids(
    connection: &Connection,
    type_name: &str,
    section: &SectionConfig,
) -> Result<Vec<String>, StoreError> {
    let mut statement = connection
        .prepare(&format!(
            "{RECORD_SELECT} WHERE r.deleted_at IS NULL AND r.type=?1 AND r.kind=?2 ORDER BY r.created_at, r.id"
        ))
        .map_err(internal)?;
    let mut records = statement
        .query_map(params![type_name, section.kind], record_from_row)
        .map_err(internal)?
        .collect::<rusqlite::Result<Vec<_>>>()
        .map_err(internal)?;
    records.retain(|r| {
        section
            .filter
            .iter()
            .all(|(field, expected)| r.fields.get(field).and_then(Value::as_str) == Some(expected))
    });
    if let Some(sort) = &section.sort {
        let key = |r: &Record| match r.fields.get(sort) {
            None | Some(Value::Null) => None,
            Some(Value::String(s)) => Some(s.clone()),
            Some(other) => Some(other.to_string()),
        };
        // Stable, so equal keys keep creation order.
        records.sort_by(|a, b| match (key(a), key(b)) {
            (Some(x), Some(y)) => x.cmp(&y),
            (Some(_), None) => std::cmp::Ordering::Less,
            (None, Some(_)) => std::cmp::Ordering::Greater,
            (None, None) => std::cmp::Ordering::Equal,
        });
    }
    Ok(records.into_iter().map(|r| r.id).collect())
}

fn accepted_rows(
    connection: &Connection,
) -> Result<HashMap<String, (String, String, String)>, StoreError> {
    let mut statement = connection
        .prepare("SELECT name, accepted_hash, accepted_body, accepted_at FROM type_configs")
        .map_err(internal)?;
    statement
        .query_map([], |r| {
            Ok((r.get::<_, String>(0)?, (r.get(1)?, r.get(2)?, r.get(3)?)))
        })
        .map_err(internal)?
        .collect::<rusqlite::Result<HashMap<_, _>>>()
        .map_err(internal)
}

fn accepted_template_rows(
    connection: &Connection,
) -> Result<HashMap<String, (String, String)>, StoreError> {
    let mut statement = connection
        .prepare("SELECT path, accepted_hash, accepted_at FROM template_files")
        .map_err(internal)?;
    statement
        .query_map([], |r| Ok((r.get::<_, String>(0)?, (r.get(1)?, r.get(2)?))))
        .map_err(internal)?
        .collect::<rusqlite::Result<HashMap<_, _>>>()
        .map_err(internal)
}

fn now_utc() -> String {
    chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true)
}

fn set_mode(path: &Path, mode: u32) -> anyhow::Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode))?;
    }
    Ok(())
}

fn migrate(connection: &mut Connection) -> anyhow::Result<()> {
    let supported = MIGRATIONS.len() as i64;
    let version: i64 = connection.pragma_query_value(None, "user_version", |r| r.get(0))?;
    if version > supported {
        return Err(anyhow!(
            "database schema version {version} is newer than supported version {supported}"
        ));
    }
    for (index, step) in MIGRATIONS.iter().enumerate().skip(version as usize) {
        let tx = connection.transaction()?;
        tx.execute_batch(step)
            .with_context(|| format!("applying schema version {}", index + 1))?;
        tx.pragma_update(None, "user_version", index as i64 + 1)?;
        tx.commit()?;
    }
    Ok(())
}

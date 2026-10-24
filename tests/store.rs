use hoja::config::{TypeFile, Workspace};
use hoja::gates::{Actor, Author};
use hoja::lint;
use hoja::model::{
    CreateDocument, CreateRecord, Decide, Decision, Outcome, SelectSection, UpdateDocument,
    UpdateRecord,
};
use hoja::store::{Store, StoreError};
use serde_json::{Map, Value, json};

const RESUME: &str = r#"
first_draft = "human"
template = "resume.typ"

[kinds.role.fields.employer]
type = "string"
required = true
person_only = true

[kinds.bullet]
label = "text"
[kinds.bullet.fields.text]
type = "text"
required = true
[kinds.bullet.fields.role]
type = "ref"
ref_kind = "role"
[kinds.bullet.fields.tags]
type = "tags"

[document.fields.summary]
type = "text"
person_only = true

[[document.sections]]
name = "experience"
kind = "bullet"
group_by = "role"
"#;

const REPORT: &str = include_str!("../examples/status-report/types/status-report.toml");

struct Fixture {
    _dir: tempfile::TempDir,
    root: std::path::PathBuf,
    store: Store,
}

impl Fixture {
    fn new() -> Self {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().to_path_buf();
        std::fs::create_dir_all(root.join("types")).unwrap();
        std::fs::write(root.join("types/resume.toml"), RESUME).unwrap();
        std::fs::write(root.join("types/status-report.toml"), REPORT).unwrap();
        let store = Store::init(&root.join("hoja.sqlite3")).unwrap();
        let fixture = Self {
            _dir: dir,
            root,
            store,
        };
        for file in fixture.files() {
            fixture
                .store
                .accept_type(&Actor::Person, &file.name, &file.hash, &fixture.files())
                .unwrap();
        }
        fixture
    }

    fn files(&self) -> Vec<TypeFile> {
        Workspace::open(&self.root).unwrap().type_files().unwrap()
    }
}

fn agent() -> Actor {
    Actor::Agent("claude".into())
}

fn obj(value: Value) -> Map<String, Value> {
    value.as_object().unwrap().clone()
}

fn create(
    store: &Store,
    actor: &Actor,
    type_name: &str,
    kind: &str,
    fields: Value,
) -> Result<Outcome<hoja::model::Record>, StoreError> {
    store.create_record(
        actor,
        CreateRecord {
            type_name: type_name.into(),
            kind: kind.into(),
            fields: obj(fields),
            note: String::new(),
        },
    )
}

fn applied<T>(outcome: Outcome<T>) -> T {
    match outcome {
        Outcome::Applied { result } => result,
        Outcome::AwaitingPerson { approval_id } => {
            panic!("expected applied, got card {approval_id}")
        }
    }
}

fn card<T: std::fmt::Debug>(outcome: Outcome<T>) -> String {
    match outcome {
        Outcome::AwaitingPerson { approval_id } => approval_id,
        Outcome::Applied { result } => panic!("expected a card, got {result:?}"),
    }
}

fn accept(
    store: &Store,
    id: &str,
    payload: Option<Value>,
) -> Result<hoja::model::Approval, StoreError> {
    store.decide(
        &Actor::Person,
        id,
        Decide {
            decision: Decision::Accept,
            reason: None,
            payload,
        },
    )
}

#[test]
fn types_take_effect_only_once_accepted() {
    let f = Fixture::new();
    let statuses = f.store.type_statuses(&f.files()).unwrap();
    assert!(statuses.iter().all(|s| s.state == "accepted"));

    // A loosened file on disk changes nothing until the person accepts its exact hash.
    let loosened = RESUME.replace("person_only = true\n\n[kinds.bullet]", "\n[kinds.bullet]");
    std::fs::write(f.root.join("types/resume.toml"), &loosened).unwrap();
    let files = f.files();
    let status = f
        .store
        .type_statuses(&files)
        .unwrap()
        .into_iter()
        .find(|s| s.name == "resume")
        .unwrap();
    assert_eq!(status.state, "changed");
    let role = applied(
        create(
            &f.store,
            &Actor::Person,
            "resume",
            "role",
            json!({"employer": "Acme"}),
        )
        .unwrap(),
    );
    let suggest = || UpdateRecord {
        version: role.version,
        fields: obj(json!({"employer": "Acme Corp"})),
        note: String::new(),
    };
    assert!(
        matches!(
            f.store.update_record(&agent(), &role.id, suggest()),
            Err(StoreError::Refused(_))
        ),
        "the accepted copy still makes employer person-only"
    );

    let new_hash = files
        .iter()
        .find(|t| t.name == "resume")
        .unwrap()
        .hash
        .clone();
    assert!(matches!(
        f.store.accept_type(&agent(), "resume", &new_hash, &files),
        Err(StoreError::Refused(_))
    ));
    assert!(matches!(
        f.store
            .accept_type(&Actor::Person, "resume", "stale-hash", &files),
        Err(StoreError::Conflict)
    ));
    f.store
        .accept_type(&Actor::Person, "resume", &new_hash, &files)
        .unwrap();
    assert_eq!(
        f.store
            .type_statuses(&f.files())
            .unwrap()
            .iter()
            .find(|s| s.name == "resume")
            .unwrap()
            .state,
        "accepted"
    );
    card(
        f.store
            .update_record(&agent(), &role.id, suggest())
            .unwrap(),
    );

    // A new file is unusable until accepted.
    std::fs::write(
        f.root.join("types/letter.toml"),
        "first_draft = \"human\"\n[kinds.p.fields.t]\ntype = \"text\"\n",
    )
    .unwrap();
    assert!(matches!(
        create(&f.store, &Actor::Person, "letter", "p", json!({"t": "x"})),
        Err(StoreError::Invalid(_))
    ));
}

#[test]
fn every_revision_records_its_author() {
    let f = Fixture::new();
    let person = applied(
        create(
            &f.store,
            &Actor::Person,
            "resume",
            "role",
            json!({"employer": "Acme"}),
        )
        .unwrap(),
    );
    assert_eq!(person.created_by.as_ref().unwrap().author, Author::Person);
    let item = applied(
        create(
            &f.store,
            &agent(),
            "status-report",
            "item",
            json!({"text": "Shipped it", "status": "done", "refs": ["!1"]}),
        )
        .unwrap(),
    );
    let by = item.created_by.unwrap();
    assert_eq!(by.author, Author::Agent);
    assert_eq!(by.agent.as_deref(), Some("claude"));
    let revisions = f.store.revisions("record", &item.id).unwrap();
    assert_eq!(revisions.len(), 1);
    assert_eq!(revisions[0].author, Author::Agent);
}

#[test]
fn agents_cannot_review_approve_delete_or_write_person_only_fields() {
    let f = Fixture::new();
    let item = applied(
        create(
            &f.store,
            &agent(),
            "status-report",
            "item",
            json!({"text": "A", "status": "done", "refs": ["!1"]}),
        )
        .unwrap(),
    );
    assert!(matches!(
        f.store.set_reviewed(&agent(), &item.id, item.version, true),
        Err(StoreError::Refused(_))
    ));
    assert!(matches!(
        f.store.delete_record(&agent(), &item.id, item.version),
        Err(StoreError::Refused(_))
    ));

    let doc = applied(
        f.store
            .create_document(
                &agent(),
                CreateDocument {
                    type_name: "status-report".into(),
                    title: "October".into(),
                    fields: obj(json!({"period": "2026-10"})),
                    note: String::new(),
                },
            )
            .unwrap(),
    );
    assert!(matches!(
        f.store.approve_document(&agent(), &doc.id, doc.version, 0),
        Err(StoreError::Refused(_))
    ));
    let summary = UpdateDocument {
        version: doc.version,
        title: None,
        fields: obj(json!({"summary": "Mine"})),
        note: String::new(),
    };
    assert!(matches!(
        f.store.update_document(&agent(), &doc.id, summary),
        Err(StoreError::Refused(_))
    ));
    let with_summary = CreateDocument {
        type_name: "status-report".into(),
        title: "Nov".into(),
        fields: obj(json!({"period": "2026-11", "summary": "x"})),
        note: String::new(),
    };
    assert!(matches!(
        f.store.create_document(&agent(), with_summary),
        Err(StoreError::Refused(_))
    ));
    assert!(matches!(
        f.store.approvals(Some("pending")).unwrap().len(),
        0
    ));
}

#[test]
fn human_types_refuse_agent_first_drafts_and_take_suggestions() {
    let f = Fixture::new();
    assert!(matches!(
        create(
            &f.store,
            &agent(),
            "resume",
            "bullet",
            json!({"text": "Wrote it all"})
        ),
        Err(StoreError::Refused(_))
    ));
    let role = applied(
        create(
            &f.store,
            &Actor::Person,
            "resume",
            "role",
            json!({"employer": "Acme"}),
        )
        .unwrap(),
    );
    let bullet = applied(
        create(
            &f.store,
            &Actor::Person,
            "resume",
            "bullet",
            json!({"text": "Led the migraton", "role": role.id}),
        )
        .unwrap(),
    );

    let suggestion = UpdateRecord {
        version: bullet.version,
        fields: obj(json!({"text": "Led the migration"})),
        note: "typo".into(),
    };
    let id = card(
        f.store
            .update_record(&agent(), &bullet.id, suggestion)
            .unwrap(),
    );
    // Asking twice doesn't open a second card.
    let again = UpdateRecord {
        version: bullet.version,
        fields: obj(json!({"text": "Led the migration"})),
        note: "typo".into(),
    };
    assert_eq!(
        card(f.store.update_record(&agent(), &bullet.id, again).unwrap()),
        id
    );
    assert_eq!(
        f.store.record(&bullet.id).unwrap().fields["text"],
        "Led the migraton"
    );

    // Agents can't decide their own cards.
    assert!(matches!(
        f.store.decide(
            &agent(),
            &id,
            Decide {
                decision: Decision::Accept,
                reason: None,
                payload: None
            }
        ),
        Err(StoreError::Refused(_))
    ));
    let decided = accept(&f.store, &id, None).unwrap();
    assert_eq!(decided.state, "accepted");
    let after = f.store.record(&bullet.id).unwrap();
    assert_eq!(after.fields["text"], "Led the migration");
    let by = after.updated_by.unwrap();
    assert_eq!(by.author, Author::AgentAccepted);
    let revision = &f.store.revisions("record", &bullet.id).unwrap()[0];
    assert_eq!(revision.approval_id.as_deref(), Some(id.as_str()));
    assert!(!revision.edited);
    // The creator is still the person.
    assert_eq!(after.created_by.unwrap().author, Author::Person);
}

#[test]
fn the_person_can_edit_a_card_but_not_widen_it() {
    let f = Fixture::new();
    let role = applied(
        create(
            &f.store,
            &Actor::Person,
            "resume",
            "role",
            json!({"employer": "Acme"}),
        )
        .unwrap(),
    );
    let bullet = applied(
        create(
            &f.store,
            &Actor::Person,
            "resume",
            "bullet",
            json!({"text": "Did a thing", "role": role.id}),
        )
        .unwrap(),
    );
    let id = card(
        f.store
            .update_record(
                &agent(),
                &bullet.id,
                UpdateRecord {
                    version: bullet.version,
                    fields: obj(json!({"text": "Did a big thing"})),
                    note: String::new(),
                },
            )
            .unwrap(),
    );
    let wider = json!({"fields": {"text": "x", "tags": ["y"]}});
    assert!(matches!(
        accept(&f.store, &id, Some(wider)),
        Err(StoreError::Invalid(_))
    ));
    let decided = accept(
        &f.store,
        &id,
        Some(json!({"fields": {"text": "Did a useful thing"}})),
    )
    .unwrap();
    assert!(decided.edited);
    assert_eq!(
        f.store.record(&bullet.id).unwrap().fields["text"],
        "Did a useful thing"
    );
    assert!(f.store.revisions("record", &bullet.id).unwrap()[0].edited);
}

#[test]
fn reviewed_content_and_stale_cards() {
    let f = Fixture::new();
    let item = applied(
        create(
            &f.store,
            &agent(),
            "status-report",
            "item",
            json!({"text": "A", "status": "done", "refs": ["!1"]}),
        )
        .unwrap(),
    );
    // Unreviewed agent-type records take direct agent edits.
    let item = applied(
        f.store
            .update_record(
                &agent(),
                &item.id,
                UpdateRecord {
                    version: item.version,
                    fields: obj(json!({"text": "B"})),
                    note: String::new(),
                },
            )
            .unwrap(),
    );
    let item = f
        .store
        .set_reviewed(&Actor::Person, &item.id, item.version, true)
        .unwrap();
    assert!(item.reviewed);
    // Reviewed ones need a card.
    let id = card(
        f.store
            .update_record(
                &agent(),
                &item.id,
                UpdateRecord {
                    version: item.version,
                    fields: obj(json!({"text": "C"})),
                    note: String::new(),
                },
            )
            .unwrap(),
    );
    // The person edits an untouched field: the card stays fresh.
    let item = applied(
        f.store
            .update_record(
                &Actor::Person,
                &item.id,
                UpdateRecord {
                    version: item.version,
                    fields: obj(json!({"project": "P"})),
                    note: String::new(),
                },
            )
            .unwrap(),
    );
    assert!(!item.reviewed, "any content change clears reviewed");
    assert!(!f.store.approval(&id).unwrap().stale);
    // Then the field the card touches: now it's stale and can't be accepted.
    applied(
        f.store
            .update_record(
                &Actor::Person,
                &item.id,
                UpdateRecord {
                    version: item.version,
                    fields: obj(json!({"text": "D"})),
                    note: String::new(),
                },
            )
            .unwrap(),
    );
    assert!(f.store.approval(&id).unwrap().stale);
    assert!(matches!(
        accept(&f.store, &id, None),
        Err(StoreError::Stale)
    ));
    let rejected = f
        .store
        .decide(
            &Actor::Person,
            &id,
            Decide {
                decision: Decision::RequestChanges,
                reason: Some("rebase on D".into()),
                payload: None,
            },
        )
        .unwrap();
    assert_eq!(rejected.state, "changes_requested");
    assert_eq!(rejected.reason.as_deref(), Some("rebase on D"));
    assert!(matches!(
        accept(&f.store, &id, None),
        Err(StoreError::Conflict)
    ));
}

#[test]
fn optimistic_versions_reject_lost_updates() {
    let f = Fixture::new();
    let role = applied(
        create(
            &f.store,
            &Actor::Person,
            "resume",
            "role",
            json!({"employer": "Acme"}),
        )
        .unwrap(),
    );
    let update = |version| UpdateRecord {
        version,
        fields: obj(json!({"employer": "Beta"})),
        note: String::new(),
    };
    applied(
        f.store
            .update_record(&Actor::Person, &role.id, update(role.version))
            .unwrap(),
    );
    assert!(matches!(
        f.store
            .update_record(&Actor::Person, &role.id, update(role.version)),
        Err(StoreError::Conflict)
    ));
}

#[test]
fn approval_follows_content_and_lints_gate_it() {
    let f = Fixture::new();
    let a = applied(
        create(
            &f.store,
            &agent(),
            "status-report",
            "item",
            json!({"text": "Shipped", "status": "done", "refs": ["!1"]}),
        )
        .unwrap(),
    );
    let b = applied(
        create(
            &f.store,
            &agent(),
            "status-report",
            "item",
            json!({"text": "Planned", "status": "next", "refs": ["nope"]}),
        )
        .unwrap(),
    );
    let doc = applied(
        f.store
            .create_document(
                &agent(),
                CreateDocument {
                    type_name: "status-report".into(),
                    title: "October".into(),
                    fields: obj(json!({"period": "2026-10"})),
                    note: String::new(),
                },
            )
            .unwrap(),
    );
    // Wrong section for the item's status.
    let bad = SelectSection {
        version: doc.version,
        record_ids: vec![b.id.clone()],
        note: String::new(),
    };
    assert!(matches!(
        f.store.select(&agent(), &doc.id, "done", bad),
        Err(StoreError::Invalid(_))
    ));
    let doc = applied(
        f.store
            .select(
                &agent(),
                &doc.id,
                "done",
                SelectSection {
                    version: doc.version,
                    record_ids: vec![a.id.clone()],
                    note: String::new(),
                },
            )
            .unwrap(),
    );
    let doc = applied(
        f.store
            .select(
                &agent(),
                &doc.id,
                "next",
                SelectSection {
                    version: doc.version,
                    record_ids: vec![b.id.clone()],
                    note: String::new(),
                },
            )
            .unwrap(),
    );

    let findings = lint::lint(&f.store.resolve(&doc.id).unwrap());
    let messages: Vec<&str> = findings.iter().map(|x| x.message.as_str()).collect();
    assert_eq!(lint::errors(&findings), 2, "{messages:?}");
    assert!(messages.iter().any(|m| m.contains("cites a commit")));
    assert!(messages.iter().any(|m| m.contains("summary")));

    let doc = applied(
        f.store
            .update_document(
                &Actor::Person,
                &doc.id,
                UpdateDocument {
                    version: doc.version,
                    title: None,
                    fields: obj(json!({"summary": "A good month."})),
                    note: String::new(),
                },
            )
            .unwrap(),
    );
    let b = applied(
        f.store
            .update_record(
                &agent(),
                &b.id,
                UpdateRecord {
                    version: b.version,
                    fields: obj(json!({"refs": ["APP-9"]})),
                    note: String::new(),
                },
            )
            .unwrap(),
    );
    let findings = lint::lint(&f.store.resolve(&doc.id).unwrap());
    assert_eq!(lint::errors(&findings), 0, "{findings:?}");

    assert!(matches!(
        f.store
            .approve_document(&Actor::Person, &doc.id, doc.version, 1),
        Err(StoreError::Invalid(_))
    ));
    let doc = f
        .store
        .approve_document(&Actor::Person, &doc.id, doc.version, 0)
        .unwrap();
    assert!(doc.approved);

    // After approval, the agent's selection changes become cards...
    let id = card(
        f.store
            .select(
                &agent(),
                &doc.id,
                "next",
                SelectSection {
                    version: doc.version,
                    record_ids: vec![],
                    note: String::new(),
                },
            )
            .unwrap(),
    );
    assert!(f.store.document(&doc.id).unwrap().approved);
    // ...and an agent edit to a selected (unreviewed) record drops the approval.
    applied(
        f.store
            .update_record(
                &agent(),
                &b.id,
                UpdateRecord {
                    version: b.version,
                    fields: obj(json!({"text": "Planned more"})),
                    note: String::new(),
                },
            )
            .unwrap(),
    );
    let doc = f.store.document(&doc.id).unwrap();
    assert!(!doc.approved);
    assert!(doc.approved_at.is_some());
    accept(&f.store, &id, None).unwrap();
    let doc = f.store.document(&doc.id).unwrap();
    assert!(
        doc.sections
            .iter()
            .all(|s| s.name != "next" || s.record_ids.is_empty())
    );
    assert_eq!(doc.updated_by.unwrap().author, Author::AgentAccepted);
}

#[test]
fn agent_direct_false_routes_creates_through_cards() {
    let f = Fixture::new();
    let strict = REPORT.replace(
        "first_draft = \"agent\"",
        "first_draft = \"agent\"\nagent_direct = false",
    );
    std::fs::write(f.root.join("types/status-report.toml"), strict).unwrap();
    let files = f.files();
    let hash = files
        .iter()
        .find(|t| t.name == "status-report")
        .unwrap()
        .hash
        .clone();
    f.store
        .accept_type(&Actor::Person, "status-report", &hash, &files)
        .unwrap();
    let id = card(
        create(
            &f.store,
            &agent(),
            "status-report",
            "item",
            json!({"text": "A", "status": "done", "refs": ["!1"]}),
        )
        .unwrap(),
    );
    assert!(
        f.store
            .records(Some("status-report"), None)
            .unwrap()
            .is_empty()
    );
    let decided = accept(&f.store, &id, None).unwrap();
    let record_id = decided.result.unwrap()["record_id"]
        .as_str()
        .unwrap()
        .to_string();
    assert_eq!(
        f.store
            .record(&record_id)
            .unwrap()
            .created_by
            .unwrap()
            .author,
        Author::AgentAccepted
    );
}

#[test]
fn refs_must_name_live_records_of_the_right_kind() {
    let f = Fixture::new();
    let role = applied(
        create(
            &f.store,
            &Actor::Person,
            "resume",
            "role",
            json!({"employer": "Acme"}),
        )
        .unwrap(),
    );
    let bullet = applied(
        create(
            &f.store,
            &Actor::Person,
            "resume",
            "bullet",
            json!({"text": "x"}),
        )
        .unwrap(),
    );
    assert!(matches!(
        create(
            &f.store,
            &Actor::Person,
            "resume",
            "bullet",
            json!({"text": "y", "role": bullet.id})
        ),
        Err(StoreError::Invalid(_))
    ));
    f.store
        .delete_record(&Actor::Person, &role.id, role.version)
        .unwrap();
    assert!(matches!(
        create(
            &f.store,
            &Actor::Person,
            "resume",
            "bullet",
            json!({"text": "y", "role": role.id})
        ),
        Err(StoreError::Invalid(_))
    ));
}

#[test]
fn newer_schemas_are_rejected() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("hoja.sqlite3");
    drop(Store::init(&path).unwrap());
    let connection = rusqlite::Connection::open(&path).unwrap();
    connection.pragma_update(None, "user_version", 99).unwrap();
    drop(connection);
    assert!(Store::init(&path).is_err());
}

#[test]
fn reviewed_and_unique_lints() {
    let f = Fixture::new();
    let body = format!(
        "{RESUME}\n[kinds.bullet.fields.claim]\ntype = \"string\"\n\n[[lints]]\nrule = \"reviewed\"\nkind = \"bullet\"\n\n[[lints]]\nrule = \"unique\"\nkind = \"bullet\"\nfield = \"claim\"\n"
    );
    std::fs::write(f.root.join("types/resume.toml"), body).unwrap();
    let files = f.files();
    let hash = files
        .iter()
        .find(|t| t.name == "resume")
        .unwrap()
        .hash
        .clone();
    f.store
        .accept_type(&Actor::Person, "resume", &hash, &files)
        .unwrap();

    let a = applied(
        create(
            &f.store,
            &Actor::Person,
            "resume",
            "bullet",
            json!({"text": "Led it", "claim": "migration"}),
        )
        .unwrap(),
    );
    let b = applied(
        create(
            &f.store,
            &Actor::Person,
            "resume",
            "bullet",
            json!({"text": "Ran it", "claim": "migration"}),
        )
        .unwrap(),
    );
    let a = f
        .store
        .set_reviewed(&Actor::Person, &a.id, a.version, true)
        .unwrap();
    let doc = applied(
        f.store
            .create_document(
                &Actor::Person,
                CreateDocument {
                    type_name: "resume".into(),
                    title: "Acme".into(),
                    fields: Map::new(),
                    note: String::new(),
                },
            )
            .unwrap(),
    );
    let ids = vec![a.id.clone(), b.id.clone()];
    applied(
        f.store
            .select(
                &Actor::Person,
                &doc.id,
                "experience",
                SelectSection {
                    version: doc.version,
                    record_ids: ids,
                    note: String::new(),
                },
            )
            .unwrap(),
    );
    let findings = lint::lint(&f.store.resolve(&doc.id).unwrap());
    let messages: Vec<&str> = findings.iter().map(|x| x.message.as_str()).collect();
    assert_eq!(findings.len(), 2, "{messages:?}");
    assert!(
        messages
            .iter()
            .any(|m| m.contains("\"Ran it\": not reviewed"))
    );
    assert!(
        messages
            .iter()
            .any(|m| m.contains("shares its claim with bullet \"Led it\""))
    );
}

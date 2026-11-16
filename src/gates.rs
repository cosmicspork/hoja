//! The rules no type file can change. The store asks `route` before every
//! write and does exactly what it answers.

use serde::Serialize;

use crate::config::{FirstDraft, TypeConfig};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Actor {
    Person,
    Agent(String),
}

/// Who a revision is attributed to.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Author {
    Person,
    Agent,
    /// An agent's card, accepted (and possibly edited) by the person.
    AgentAccepted,
}

impl Author {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Person => "person",
            Self::Agent => "agent",
            Self::AgentAccepted => "agent_accepted",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Op {
    Create,
    Update,
    Select,
}

/// A write, described by what the gates need to know about it.
#[derive(Debug, Clone, Copy)]
pub struct Write<'a> {
    pub op: Op,
    /// `None` for a document; otherwise the record's kind.
    pub kind: Option<&'a str>,
    /// The record is reviewed, or the document was approved at least once.
    pub settled: bool,
    /// Field names the write sets or clears.
    pub fields: &'a [String],
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Route {
    Direct,
    Card,
    Refused(String),
}

/// Person-only actions: marking content reviewed, approving documents,
/// deciding cards, accepting type files, deleting records.
pub fn person_only_action(actor: &Actor, action: &str) -> Result<(), String> {
    match actor {
        Actor::Person => Ok(()),
        Actor::Agent(_) => Err(format!("only the person can {action}")),
    }
}

pub fn route(actor: &Actor, config: &TypeConfig, write: Write<'_>) -> Route {
    if *actor == Actor::Person {
        return Route::Direct;
    }
    let declared = match write.kind {
        None => Some(&config.document.fields),
        Some(kind) => config.kind(kind).map(|k| &k.fields),
    };
    if let Some(declared) = declared
        && let Some(name) = write
            .fields
            .iter()
            .find(|name| declared.get(*name).is_some_and(|f| f.person_only))
    {
        return Route::Refused(format!("`{name}` is written only by the person"));
    }
    if config.first_draft == FirstDraft::Human {
        return if write.op == Op::Create {
            Route::Refused(
                "this type's first draft is the person's; agents may suggest changes to existing records and documents".into(),
            )
        } else {
            Route::Card
        };
    }
    let kind_direct = write
        .kind
        .and_then(|k| config.kind(k))
        .is_none_or(|k| k.agent_direct);
    if !config.agent_direct || !kind_direct || write.settled {
        Route::Card
    } else {
        Route::Direct
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn config(first_draft: &str, agent_direct: bool) -> TypeConfig {
        TypeConfig::parse(&format!(
            r#"
first_draft = "{first_draft}"
agent_direct = {agent_direct}
[kinds.item.fields.text]
type = "text"
[kinds.item.fields.secret]
type = "text"
person_only = true
[document.fields.summary]
type = "text"
person_only = true
"#
        ))
        .unwrap()
    }

    fn agent() -> Actor {
        Actor::Agent("claude".into())
    }

    fn write<'a>(op: Op, kind: Option<&'a str>, settled: bool, fields: &'a [String]) -> Write<'a> {
        Write {
            op,
            kind,
            settled,
            fields,
        }
    }

    #[test]
    fn the_person_always_writes_directly() {
        let c = config("human", false);
        let fields = ["secret".to_string()];
        let w = write(Op::Update, Some("item"), true, &fields);
        assert_eq!(route(&Actor::Person, &c, w), Route::Direct);
    }

    #[test]
    fn agents_never_write_person_only_fields() {
        for c in [config("human", true), config("agent", true)] {
            let secret = ["secret".to_string()];
            let summary = ["summary".to_string()];
            for w in [
                write(Op::Update, Some("item"), false, &secret),
                write(Op::Create, Some("item"), false, &secret),
                write(Op::Update, None, false, &summary),
                write(Op::Create, None, false, &summary),
            ] {
                assert!(matches!(route(&agent(), &c, w), Route::Refused(_)), "{w:?}");
            }
        }
    }

    #[test]
    fn human_types_refuse_agent_first_drafts_and_card_everything_else() {
        let c = config("human", true);
        let text = ["text".to_string()];
        assert!(matches!(
            route(&agent(), &c, write(Op::Create, Some("item"), false, &text)),
            Route::Refused(_)
        ));
        assert!(matches!(
            route(&agent(), &c, write(Op::Create, None, false, &[])),
            Route::Refused(_)
        ));
        assert_eq!(
            route(&agent(), &c, write(Op::Update, Some("item"), false, &text)),
            Route::Card
        );
        assert_eq!(
            route(&agent(), &c, write(Op::Select, None, false, &[])),
            Route::Card
        );
    }

    #[test]
    fn agent_types_allow_direct_drafts_until_settled() {
        let c = config("agent", true);
        let text = ["text".to_string()];
        assert_eq!(
            route(&agent(), &c, write(Op::Create, Some("item"), false, &text)),
            Route::Direct
        );
        assert_eq!(
            route(&agent(), &c, write(Op::Update, Some("item"), false, &text)),
            Route::Direct
        );
        assert_eq!(
            route(&agent(), &c, write(Op::Update, Some("item"), true, &text)),
            Route::Card
        );
        assert_eq!(
            route(&agent(), &c, write(Op::Select, None, true, &[])),
            Route::Card
        );
    }

    #[test]
    fn a_kind_can_require_cards_for_agent_writes() {
        let c = TypeConfig::parse(
            r#"
first_draft = "agent"
[kinds.risk]
agent_direct = false
[kinds.risk.fields.text]
type = "text"
[kinds.issue.fields.text]
type = "text"
"#,
        )
        .unwrap();
        let text = ["text".to_string()];
        assert_eq!(
            route(&agent(), &c, write(Op::Create, Some("risk"), false, &text)),
            Route::Card
        );
        assert_eq!(
            route(&agent(), &c, write(Op::Create, Some("issue"), false, &text)),
            Route::Direct
        );
    }

    #[test]
    fn agent_direct_false_only_tightens() {
        let c = config("agent", false);
        assert_eq!(
            route(&agent(), &c, write(Op::Create, Some("item"), false, &[])),
            Route::Card
        );
        assert_eq!(
            route(&agent(), &c, write(Op::Select, None, false, &[])),
            Route::Card
        );
    }
}

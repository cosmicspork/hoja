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
    /// A new record fills its kind's `proposal_source` field.
    pub cites: bool,
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
    // A kind with a proposal source takes agents' new records only as
    // proposals the person accepts, each citing where its facts come from.
    if write.op == Op::Create
        && let Some(source) = write
            .kind
            .and_then(|k| config.kind(k))
            .and_then(|k| k.proposal_source.as_deref())
    {
        return if write.cites {
            Route::Card
        } else {
            Route::Refused(format!(
                "a proposed record cites where its facts come from in `{source}`"
            ))
        };
    }
    let declared = match write.kind {
        None => Some(&config.document.fields),
        Some(kind) => config.kind(kind).map(|k| &k.fields),
    };
    let person_only = declared.and_then(|declared| {
        write
            .fields
            .iter()
            .find(|name| declared.get(*name).is_some_and(|f| f.person_only))
    });
    if let Some(name) = person_only {
        // A new entry may carry the person's ratings (an existing log being
        // moved in, say) as a proposal they accept once. Changing one never
        // comes from an agent.
        if write.op != Op::Create || config.first_draft == FirstDraft::Human {
            return Route::Refused(format!("`{name}` is written only by the person"));
        }
        return Route::Card;
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
            cites: false,
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
    fn agents_never_change_person_only_fields() {
        for c in [config("human", true), config("agent", true)] {
            let secret = ["secret".to_string()];
            let summary = ["summary".to_string()];
            for w in [
                write(Op::Update, Some("item"), false, &secret),
                write(Op::Update, None, false, &summary),
            ] {
                assert!(matches!(route(&agent(), &c, w), Route::Refused(_)), "{w:?}");
            }
        }
        let c = config("human", true);
        let secret = ["secret".to_string()];
        assert!(matches!(
            route(
                &agent(),
                &c,
                write(Op::Create, Some("item"), false, &secret)
            ),
            Route::Refused(_)
        ));
    }

    #[test]
    fn a_new_entry_with_person_only_fields_is_always_a_card() {
        let c = config("agent", true);
        let both = ["text".to_string(), "secret".to_string()];
        let summary = ["summary".to_string()];
        assert_eq!(
            route(&agent(), &c, write(Op::Create, Some("item"), false, &both)),
            Route::Card
        );
        assert_eq!(
            route(&agent(), &c, write(Op::Create, None, false, &summary)),
            Route::Card
        );
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

    fn facts(first_draft: &str) -> TypeConfig {
        TypeConfig::parse(&format!(
            r#"
first_draft = "{first_draft}"
[kinds.degree]
proposal_source = "source"
[kinds.degree.fields.field]
type = "string"
person_only = true
[kinds.degree.fields.source]
type = "text"
person_only = true
[kinds.bullet.fields.text]
type = "text"
"#
        ))
        .unwrap()
    }

    fn proposal<'a>(fields: &'a [String], cites: bool) -> Write<'a> {
        Write {
            cites,
            ..write(Op::Create, Some("degree"), false, fields)
        }
    }

    #[test]
    fn agents_propose_cited_records_of_a_proposal_kind_as_cards() {
        let fields = ["field".to_string(), "source".to_string()];
        for c in [facts("human"), facts("agent")] {
            assert_eq!(route(&agent(), &c, proposal(&fields, true)), Route::Card);
            assert!(matches!(
                route(&agent(), &c, proposal(&fields[..1], false)),
                Route::Refused(_)
            ));
            assert_eq!(
                route(&Actor::Person, &c, proposal(&fields[..1], false)),
                Route::Direct
            );
        }
    }

    #[test]
    fn a_proposal_kind_opens_nothing_but_new_cited_records() {
        let c = facts("human");
        let field = ["field".to_string()];
        let text = ["text".to_string()];
        // Once accepted, a fact's person-only fields stay out of agents' reach.
        let update = Write {
            cites: true,
            ..write(Op::Update, Some("degree"), false, &field)
        };
        assert!(matches!(route(&agent(), &c, update), Route::Refused(_)));
        // Other kinds of a human type still refuse agent first drafts.
        let bullet = Write {
            cites: true,
            ..write(Op::Create, Some("bullet"), false, &text)
        };
        assert!(matches!(route(&agent(), &c, bullet), Route::Refused(_)));
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

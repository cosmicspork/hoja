# hoja

A local tool for working safely with agents to build structured documents.

People and agents edit the same records. Every change records who made it. Agents draft where a document type lets them, and otherwise suggest; the person reviews, accepts, and approves. Typst renders the result.

hoja is a generic engine. Document types are local configuration: a monthly project status report ships here as the example, and resumes or anything else live in your own private workspace. It is a companion to [Tracon](https://github.com/cosmicspork/tracon), like [workboard](https://github.com/cosmicspork/workboard): hoja holds no forge or Jira credentials, so an agent gathers facts through Tracon and submits them here through the CLI.

## The rules

These are in code, not configuration. A type file can tighten them and has no way to loosen them.

- **Every revision has an author:** `person`, `agent` (with the agent's name), or `agent_accepted`, an agent's suggestion the person accepted, flagged when the person edited it first.
- **Only the person** marks content reviewed, approves a document, decides cards, accepts type files, deletes records, and writes fields a type marks `person_only`. Agents can't even suggest a `person_only` field.
- **The first draft belongs to whoever the type says.** In a `first_draft = "human"` type (a resume), agents can't create records or documents; they can only suggest changes to what the person wrote. In a `first_draft = "agent"` type (a status report), agents write unreviewed records and drafts directly.
- **Settled content needs a card.** Once a record is reviewed or a document has been approved, an agent's change becomes a card for the person, even in an agent-drafted type. `agent_direct = false` sends every agent write through a card.
- **Changes reset review.** Editing a record clears its reviewed mark. A document is approved only while its content matches what the person approved, so any later edit to it, or to a record it selects, returns it to draft. Exports of drafts are named `-DRAFT`.
- **Approval waits on lints.** A document with error-level lint findings can't be approved.
- **Cards apply to what the agent saw.** A card records the version the agent read. If the fields it touches changed since, it is stale and can only be rejected or sent back.
- **Type files need the person's acceptance.** hoja runs on the accepted copy of each `types/*.toml`. A new or edited file shows its diff in the UI and takes effect only when the person accepts those exact contents.

### Cards

An agent request that needs the person returns immediately:

```json
{ "state": "awaiting_person", "approval_id": "3cfc…" }
```

`hoja wait <approval_id>` long-polls for up to 45 seconds and returns `still_waiting` (ask again with the same id), `accepted` (with the result), `rejected` or `changes_requested` (with the person's reason). This is the same non-blocking shape as Tracon's approval queue, so cards can move there once Tracon supports plugins. Identical pending requests share one card.

### What this is not

hoja is a protocol between you and an agent that uses its CLI. It is not a sandbox. The server listens only on loopback; person routes require the page's own Origin; the agent API rejects any request with a browser Origin and requires an `X-Hoja-Agent` header, so web pages can't reach either. An agent running on your machine as you can still read the database file or forge headers with `curl`. Run agents you don't trust in an environment that can't reach the workspace, as Tracon's managed sessions do.

## Install and run

Requires Rust, Bun, and the [Typst](https://typst.app) CLI on `PATH`.

```sh
bun install --cwd spa
bun run --cwd spa build
cargo install --path . --locked
hoja serve --workspace examples/status-report
```

Open `http://localhost:4520/`, accept the type under **Types**, and you have an empty status report workspace. Build the frontend before compiling Rust: the UI is embedded in the binary.

`--port` changes the port (default 4520), `--typst` (or `HOJA_TYPST`) picks the Typst binary, and `HOJA_WORKSPACE` can stand in for `--workspace`. Under systemd, `contrib/systemd/` has a socket-activated user service: hoja starts the first time its port is opened.

## Workspaces

A workspace is a directory:

```text
hoja.toml          owner = "Your Name"   (used in export file names)
types/*.toml       document types
templates/*.typ    Typst templates
fonts/             optional; passed to Typst with --font-path
hoja.sqlite3       the source of truth (created on first run, mode 0600)
out/               exports
```

Keep separate workspaces for separate contexts, such as career records and work reports, so they never share a database. Back up `hoja.sqlite3` with its `-wal` and `-shm` files while the server is stopped.

### Type files

A type declares record kinds and their fields, the document's own fields, its sections, lints, and how files are named. See [`examples/status-report/types/status-report.toml`](examples/status-report/types/status-report.toml) for a complete one.

- `first_draft`: `human` or `agent`, as above. `agent_direct = false` tightens an agent type.
- `[kinds.<kind>.fields.<name>]`: `type` is `string`, `text`, `tags`, `list`, `enum` (with `options`), `date` (`YYYY`, `YYYY-MM` or `YYYY-MM-DD`), `bool`, or `ref` (with `ref_kind`). Fields can be `required` and `person_only`, and carry a `title` and `help`.
- `[document.fields.<name>]`: the per-document text, such as a summary.
- `[[document.sections]]`: `name`, `kind`, an optional `where` filter on field values, `group_by` (a ref field; items render grouped under the referenced record), and `item`, the Markdown/HTML line pattern such as `"{text} ({refs})"`. A document selects, orders, and omits records per section; it never edits them.
- `[[lints]]`: `forbid` (a regex over text fields), `require`, `match` (every entry matches a regex, with an optional `min`), `max_words`, `max_items`, `reviewed` (every selected record of a kind is reviewed), and `unique` (no two selected records share a field value, such as one bullet per accomplishment). Each takes `level = "error"` (the default, blocks approval) or `"warn"`, and an optional `message`.
- `template`, `exports` (`pdf`, `md`, `html`), and `filename` with `{owner}`, `{type}`, `{title}`, `{date}` and `{doc.<field>}`.

### Templates

hoja compiles the type's template in a scratch directory holding a copy of `templates/` and a `data.json`:

```typst
#let data = json("data.json")
// data.owner, data.date, data.document.{title, fields, approved}
// data.sections.<name>.items: records with their fields, id and reviewed
// data.sections.<name>.groups: ({ref, items}) when the section has group_by
```

Templates never see the workspace or the database. Compile errors show in the UI and the CLI as Typst prints them. Markdown and HTML exports are built in from the sections' `item` patterns.

## For agents: the CLI

Every command talks to the running server and sends `--agent NAME` (or `HOJA_AGENT`), which is recorded on every revision and card. Output is JSON.

```sh
export HOJA_AGENT=claude
hoja types                                   # accepted types and their fields
hoja records --type status-report
hoja record show <id>                        # with its revision history
hoja record create --type status-report --kind item \
  --set text="Shipped offline sync" --set status=done --set refs='["!218"]'
hoja record edit <id> --version 3 --set text="..." --note "why"
hoja import sample.jsonl                     # {"type", "kind", "fields", "note"?} per line
hoja doc create --type status-report --title "October" --set period=2026-10
hoja doc select <doc> --version 4 --section done --records <id>,<id>
hoja doc show <doc>                          # selection, lint findings, export names
hoja build <doc> --format md                 # writes into the workspace's out/
hoja wait <approval_id>
```

`--version` is the version you read. Writes return `{"state": "applied", "result": …}` or a card. A refusal (`"error": "refused"`) is a decision: don't route around it.

## Development

```sh
bun run --cwd spa build
cargo build --locked
cargo test --locked
cargo clippy --all-targets -- -D warnings
bun run --cwd spa check
bun test --cwd spa
```

`bun run --cwd spa dev` serves the UI with hot reload and proxies `/api` to a `hoja serve` on port 4520. Gates live in `src/gates.rs` and are enforced in `src/store.rs`; type parsing and value normalization are in `src/config.rs`; lints in `src/lint.rs`; rendering in `src/render.rs`. The HTTP tests in `tests/http.rs` render a real PDF when `typst` is on `PATH`.

## License

[MIT](LICENSE)

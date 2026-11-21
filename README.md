# hoja

A local tool for working safely with agents to build structured documents.

People and agents edit the same records. Every change records who made it. Agents draft where a document type lets them, and otherwise suggest; the person reviews, accepts, and approves. Typst renders the result.

hoja is a generic engine. Document types are local configuration: a project's charter, RAID log and monthly status report ship here as the example, and resumes or anything else live in your own private workspace. It is a companion to [Tracon](https://github.com/cosmicspork/tracon), like [workboard](https://github.com/cosmicspork/workboard): hoja holds no forge or Jira credentials, so an agent gathers facts through Tracon and submits them here through the CLI.

## The rules

These are in code, not configuration. A type file can tighten them and has no way to loosen them.

- **Every revision has an author:** `person`, `agent` (with the agent's name), or `agent_accepted`, an agent's suggestion the person accepted, flagged when the person edited it first.
- **Only the person** marks content reviewed, approves a document, decides cards, accepts type files, deletes records, and writes fields a type marks `person_only`. Agents can't even suggest a change to a `person_only` field. A new record or document an agent proposes may carry one (an existing log being moved in, with your ratings), and then it always arrives as a card, in any type; a `first_draft = "human"` type still refuses agent creates outright.
- **The first draft belongs to whoever the type says.** In a `first_draft = "human"` type (a resume), agents can't create records or documents; they can only suggest changes to what the person wrote. In a `first_draft = "agent"` type (a status report), agents write unreviewed records and drafts directly.
- **Settled content needs a card.** Once a record is reviewed or a document has been approved, an agent's change becomes a card for the person, even in an agent-drafted type. `agent_direct = false` sends every agent write through a card.
- **Changes reset review.** Editing a record clears its reviewed mark. A document is approved only while its content matches what the person approved, so any later edit to it, or to a record it selects, returns it to draft. Exports of drafts are named `-DRAFT`.
- **Approval covers what the person saw.** The page sends the content hash it showed. If anything changed since, a selected record, the template, the type's layout, approval is refused and the page reloads. The content hash ignores the order fields are stored in.
- **Approval waits on lints.** A document with error-level lint findings can't be approved. Lints are checked against the content being approved, in the same transaction.
- **Cards apply to what the agent saw.** A card records the version the agent read. If the fields it touches changed since, it is stale and can only be rejected or sent back.
- **Type files and templates need the person's acceptance.** hoja runs on the accepted copy of each `types/*.toml` and of every file under `templates/`. A new or edited file shows its diff in the UI (a template also renders a preview) and takes effect only when the person accepts those exact contents. A document's content hash includes every accepted template file its template reaches, so accepting a template change returns the documents it renders to draft, and editing a file on disk changes nothing until it is accepted.

### Reviewing in small diffs

hoja never asks for a whole-document proofread. Cards show agent changes as word diffs (list fields entry by entry). A record that changed since you marked it reviewed shows just what changed. The document page's **Provenance** view marks every word by where it came from, worked out from the revision history:

- unmarked: you wrote it, or an agent wrote it and you have since marked the record reviewed;
- light highlight: agent wording you accepted on a card, at the words that changed;
- strong highlight: agent text you haven't reviewed;
- a wavy underline on anything changed since you last approved the document, with removed words struck through.

hoja renders this view itself, so no template can hide it. The PDF tab is a layout check; the words to read are the highlighted ones.

### Cards

An agent request that needs the person returns immediately:

```json
{ "state": "awaiting_person", "approval_id": "3cfc…" }
```

`hoja wait <approval_id>` long-polls for up to 45 seconds and returns `still_waiting` (ask again with the same id), `accepted` (with the result), `rejected` or `changes_requested` (with the person's reason). This is the same non-blocking shape as Tracon's approval queue, so cards can move there once Tracon supports plugins. Identical pending requests share one card.

### Cards in Tracon's queue

When hoja runs as a Tracon plugin, a card can be raised in the operator's Tracon queue instead of hoja's own (`src/node_cards.rs`). The node gives the plugin a socket for calling it back; hoja asks it for a card (`approval/request`) showing the agent's change as a word diff per field, with the agent's note, and reads the decision (`approval/status`). The card is still hoja's: it is linked to the Tracon approval, and once the operator decides there, hoja decides it here as the person. An approval accepts it, so the revision is `agent_accepted` under the agent's name. A rejection, a request for changes or an expiry rejects it or sends it back with the operator's words. A card whose target moved since the agent read it is rejected as stale, never applied. Raised for an agent's call through Tracon, the card carries that call's signed caller context, so the node files it with the agent's session, and the agent's name is taken from the same context rather than from the agent. hoja's own queue is unchanged for standalone use.

A card opened by one of hoja's tools (`hoja__record_edit` on a RAID risk, say) is raised this way before the tool answers, so the agent's answer is the usual `awaiting_person` with hoja's `approval_id`, and `hoja__card_status` reports the decision once it has settled here. hoja asks the node about the cards it raised every two seconds while it runs, and again when it next starts, since the links are kept in its database. The card also stays in hoja's page, and the first decision, there or in Tracon, is the one that counts; a card decided in hoja's page is not withdrawn from Tracon's queue, where it waits until it expires and then changes nothing. The relay needs a node that offers `approval/request` and `approval/status` (Tracon 0.56 or later), which the manifest names in `node_methods` so the node passes `--node-socket`. Without that socket, or if the node refuses or can't be reached, the card waits in hoja's page only, and the tool answers the same. A card opened through the CLI (`agent_listen`) carries no signed context and waits in hoja's page.

### What this is not

hoja is a protocol between you and an agent that uses its CLI or tools. It is not a sandbox. The server listens only on loopback; person routes require the page's own Origin; the agent API rejects any request with a browser Origin and requires an `X-Hoja-Agent` header, so web pages can't reach either. An agent running on your machine as you can still read the database file or forge headers with `curl`. Run agents you don't trust in an environment that can't reach the workspace, as Tracon's managed sessions do.

## Install and run

Requires Rust, Bun, and the [Typst](https://typst.app) CLI on `PATH`.

```sh
bun install --cwd spa
bun run --cwd spa build
cargo install --path . --locked
hoja serve --workspace examples/project
```

Open `http://localhost:4520/`, review and accept the types and templates under **Types**, and you have an empty project workspace. Build the frontend before compiling Rust: the UI is embedded in the binary.

The example has three types that work together:

- **Project charter** (`first_draft = "human"`): sectioned prose you write, one Markdown field per section, plus milestone and stakeholder records and the initial risks picked from the RAID log. Agents suggest edits to a section as cards.
- **RAID log** (`first_draft = "agent"`): risks, assumptions, issues and dependencies, plus the decisions that settle them. Likelihood, severity, owner and status are yours only. Agents log unrated issues and dependencies directly (unreviewed); their risks, assumptions and decisions, and any entry that arrives already rated, come as cards. Each section holds every entry of its kind, ordered by ID, so nothing is left out by selection; close entries rather than deleting them. You publish the log's PDF rather than approve it; approving snapshots a version you sent out.
- **Monthly status report** (`first_draft = "agent"`): done, in progress and next items an agent gathers, plus the top risks and open issues selected from the RAID log. Changing a RAID entry the report shows returns the report to draft.

`HOJA_AGENT=claude hoja import examples/project/sample.jsonl` loads status items and RAID entries the way an agent would: the risk comes to you as a card. `samples/*.json` hold invented content for template previews.

`--port` changes the port (default 4520), `--typst` (or `HOJA_TYPST`) picks the Typst binary, and `HOJA_WORKSPACE` can stand in for `--workspace`. Under systemd, `contrib/systemd/` has a socket-activated user service: hoja starts the first time its port is opened.

## As a Tracon plugin

hoja also runs as a [Tracon](https://github.com/cosmicspork/tracon) plugin with a UI. Once a release exists:

```sh
tracon plugin add cosmicspork/hoja
```

The node starts `hoja serve --socket <path> --data <dir>` for each channel, serves hoja's page itself, and shows it in Tracon's **Plugins** section. The page's requests reach hoja through the node, each with a context the node signs for the operator; hoja serves its person API only to requests carrying a valid one, in place of the Origin check above. hoja takes no credentials and reaches no network.

Agents on the channel get hoja's tools, `hoja__types`, `hoja__record_edit` and the rest (see [For agents: the tools](#for-agents-the-tools)). Each call carries a context the node signs, and hoja checks it before running anything: it must be for this start of hoja, this tool and exactly these arguments, fresh and not replayed, and from an agent. The author hoja records is the harness the node names in it (`claude`, or `external` for a harness you run yourself), not a name the agent picked. The tools need no Tracon approval of their own, since hoja's gates already decide what an agent's write does: it lands where the type lets agents write directly, and otherwise becomes a card you decide in Tracon's queue (see [Cards in Tracon's queue](#cards-in-tracons-queue)) or in hoja's page. If the node does ask you about a call, the card's preview says so; allowing it lets the call reach hoja, and does not accept the change.

The node starts it with an empty environment, so it is configured by an optional `<data>/plugin.toml`:

```toml
# The workspace. Default: the data directory itself, which then holds
# hoja.toml, types/, templates/ and hoja.sqlite3. Use an absolute path
# (there is no HOME to expand ~); a relative one is from the data directory.
workspace = "/home/you/hoja"
# The Typst compiler. Default: the first of /home/linuxbrew/.linuxbrew/bin/typst,
# /opt/homebrew/bin/typst, /usr/local/bin/typst and /usr/bin/typst that exists.
typst = "/usr/bin/typst"
# Serve the agent API on this loopback address, for the hoja CLI. Default: off.
agent_listen = "127.0.0.1:4520"
```

Pointing `workspace` at an existing workspace, such as `~/hoja`, keeps its data in place. Stop the standalone server first (`systemctl --user stop hoja.socket hoja.service`), since `agent_listen` on 4520 would collide with it and two servers shouldn't share one database.

The socket's HTTP API is the operator's alone, so it refuses `/api/agent/*`; agents use the tools. With `agent_listen` set, the CLI and `hoja mcp` also work against that port (`HOJA_URL` if it isn't 4520) with the same agent checks as standalone; it serves nothing else. Inside Tracon the page is framed and may not display PDF previews; each preview has a download link.

## Workspaces

A workspace is a directory:

```text
hoja.toml          owner = "Your Name"   (used in export file names)
types/*.toml       document types
templates/         Typst templates and anything they include (images, shared .typ files)
samples/*.json     optional invented content for previewing a template before accepting it
fonts/             optional; passed to Typst with --font-path
hoja.sqlite3       the source of truth (created on first run, mode 0600)
out/               exports
```

Keep separate workspaces for separate contexts, such as career records and work reports, so they never share a database. Back up `hoja.sqlite3` with its `-wal` and `-shm` files while the server is stopped.

### Type files

A type declares record kinds and their fields, the document's own fields, its sections, lints, and how files are named. See [`examples/project/types/status-report.toml`](examples/project/types/status-report.toml) for a complete one.

- `first_draft`: `human` or `agent`, as above. `agent_direct = false` tightens an agent type, and `[kinds.<kind>] agent_direct = false` tightens one kind (a RAID log's risks).
- `[kinds.<kind>.fields.<name>]`: `type` is `string`, `text`, `tags`, `list`, `enum` (with `options`), `date` (`YYYY`, `YYYY-MM` or `YYYY-MM-DD`), `bool`, or `ref` (with `ref_kind`). Fields can be `required` and `person_only`, and carry a `title` and `help`.
- `[document.fields.<name>]`: the per-document text, such as a summary.
- `[[document.sections]]`: `name`, `kind`, an optional `type` to select records of another accepted type (a status report picking from the RAID log), an optional `where` filter on field values, `group_by` (a ref field of this type; items render grouped under the referenced record), and `item`, the Markdown/HTML line pattern such as `"{text} ({refs})"`. A document selects, orders, and omits records per section; it never edits them. `include = "all"` makes a section hold every live record of its kind matching `where`, ordered by an optional `sort` field (records without one last), instead of a selection; nobody picks its records by hand, and a new or changed record returns an approved document to draft. Records from another type keep that type's gates, and its records are checked when selected, since it is accepted separately. Lints name this type's kinds only.
- `[[lints]]`: `forbid` (a regex over text fields), `require`, `match` (every entry matches a regex, with an optional `min`), `max_words`, `max_items`, `reviewed` (every selected record of a kind is reviewed), and `unique` (no two selected records share a field value, such as one bullet per accomplishment). Each takes `level = "error"` (the default, blocks approval) or `"warn"`, and an optional `message`.
- `template`, `exports` (`pdf`, `md`, `html`), and `filename` with `{owner}`, `{type}`, `{title}`, `{date}` and `{doc.<field>}`.

### Templates

hoja compiles the type's template in a scratch directory holding `data.json` and the accepted copies of the template files it reaches: the entry file, then any accepted file named by a string literal in a reachable `.typ` file (`#import "parts/style.typ"`, `image("logo.png")`), resolved the way Typst resolves paths. Nothing else is there, so a template can't read the workspace, the database, or a file nobody accepted, and a path built at runtime fails to compile.

```typst
#let data = json("data.json")
// data.owner, data.date, data.document.{title, fields, approved}
// data.sections.<name>.items: records with their fields, id and reviewed
// data.sections.<name>.groups: ({ref, items}) when the section has group_by
```

`hoja.typ` and `data.json` are reserved names. hoja supplies `hoja.typ` itself (see Markdown below); a file in a subdirectory imports it as `"/hoja.typ"`. Compile errors show in the UI and the CLI as Typst prints them. Markdown and HTML exports are built in from the sections' `item` patterns.

### Markdown

`text` fields are Markdown: paragraphs, `#` headings, `-` and `1.` lists, `*emphasis*`, `**strong**`, `` `code` ``, quotes, and http(s) or mailto links. Raw HTML stays literal text. The editor has a live preview, and the HTML export renders it. Templates get each text field's parsed form next to the raw string, under `_md`, and render it with hoja's helper:

```typst
#import "hoja.typ": md
#md(data.document, "summary")        // a document field
#for item in section.items [ #md(item, "description") ]
```

`md` never evaluates text as Typst. Markdown headings start one level below the template's own (`heading-offset: 1`).

Reviewing a template under **Types** shows its diff and renders a preview with that file swapped in: from `samples/<type>.json` when it exists (`{"title", "fields", "sections": {"<name>": [{"fields": {...}}]}}`), from placeholders when it doesn't, or from one of your documents.

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

## For agents: the tools

The same operations are MCP tools, one per agent API route, with the same names, inputs and answers. One table in `src/ops.rs` defines them: the HTTP routes are built from it, both MCP modes list it, and `tests/parity.rs` fails if a route has no tool, a tool has no route, or a person route is neither the person's copy of a tool nor listed with the reason it isn't one.

| Tool | Route under `/api/agent` | CLI |
|---|---|---|
| `types` | `GET /types` | `hoja types` |
| `records` | `GET /records?type=&kind=` | `hoja records` |
| `record_show` | `GET /records/{id}` | `hoja record show` |
| `record_create` | `POST /records` | `hoja record create` |
| `record_edit` | `POST /records/{id}` | `hoja record edit` |
| `documents` | `GET /documents?type=` | `hoja docs` |
| `document_show` | `GET /documents/{id}` | `hoja doc show` |
| `document_create` | `POST /documents` | `hoja doc create` |
| `document_edit` | `POST /documents/{id}` | `hoja doc edit` |
| `document_select` | `POST /documents/{id}/sections/{section}` | `hoja doc select` |
| `document_build` | `POST /documents/{id}/build` | `hoja build` |
| `card_status` | `GET /approvals/{approval_id}?wait=` | `hoja wait` |

A tool's arguments are the route's path parameters (`id`, `section`, `approval_id`) and its query or JSON body, in one object. Its result is the API's JSON body, flagged as an error when the API would answer with one (`conflict`, `stale`, `refused`, `invalid_request`, `not_found`). `note` is Markdown, shown to you on any card the call opens.

What only you decide has no route and no tool: marking records reviewed, approving documents, deciding cards, accepting type and template files, and deleting records.

Two ways to reach them:

- **Through Tracon**, as `hoja__<tool>`, when hoja runs as a plugin (above).
- **`hoja mcp`**, a stdio MCP server for any MCP client. Each tool is sent to the running server as the agent `--agent` (or `HOJA_AGENT`) names, exactly as the CLI sends it, so the server's gates and your page apply:

  ```sh
  claude mcp add hoja -e HOJA_AGENT=claude -- hoja mcp
  ```

## Development

```sh
bun run --cwd spa build
cargo build --locked
cargo test --locked
cargo clippy --all-targets -- -D warnings
bun run --cwd spa check
bun test --cwd spa
```

`bun run --cwd spa dev` serves the UI with hot reload and proxies `/api` to a `hoja serve` on port 4520. Plugin mode is in `src/plugin.rs` and the signed-context check in `src/contract.rs`; `tests/plugin.rs` runs the built binary as a node would. Gates live in `src/gates.rs` and are enforced in `src/store.rs`; type parsing and value normalization are in `src/config.rs`; lints in `src/lint.rs`; rendering in `src/render.rs`; the template gate in `src/templates.rs`; Markdown in `src/markdown.rs`; the provenance view in `src/provenance.rs`. The HTTP tests in `tests/http.rs` render real PDFs, including every example template from its samples and from placeholders, when `typst` is on `PATH`.

## Releases

Releases are cut with [release-please](https://github.com/googleapis/release-please). Each merge to `main` updates a release PR that bumps the version in `Cargo.toml`, `Cargo.lock`, `tracon-plugin.toml` and `spa/package.json` together and writes `CHANGELOG.md` from the conventional-commit PR titles (`feat` bumps the minor version, `fix` the patch, and before 1.0 a breaking change bumps the minor). Merging the release PR tags `v<version>`, creates the GitHub release, and the release workflow attaches `hoja-<version>-<target>.tar.gz` for each target (holding `tracon-plugin.toml`, `bin/hoja` and `ui/`) with `checksums.txt` and build provenance attestations.

## License

[MIT](LICENSE)

// RAID log: a summary strip, then one table per kind, landscape.
#import "hoja.typ": md
#import "common.typ": *

#let data = json("data.json")
#let doc = data.document
#let f = doc.fields
#show: body => setup(data, body, flipped: true)

#title-block(data, subtitle: [RAID log · #f.at("project", default: "")])

// Target date, as-of date, review cadence and tracking keys, when set.
#{
  let facts = (
    ("Target date", field(f, "target_date")),
    ("As of", field(f, "as_of")),
    ("Reviewed at", field(f, "review")),
    ("Tracking", if f.at("refs", default: ()).len() > 0 { f.refs.map(raw).join(", ") }),
  ).filter(p => p.at(1) != none)
  if facts.len() > 0 {
    block(below: 1.1em, grid(
      columns: facts.len(),
      column-gutter: 2.2em,
      ..facts.map(p => stack(
        spacing: 0.35em,
        text(size: 7.5pt, weight: "bold", fill: muted, upper(p.at(0))),
        text(size: 10pt, p.at(1)),
      )),
    ))
  }
}

// Statuses that need no more attention; everything else counts as open.
#let settled = ("closed", "resolved", "met", "valid")

// One box per section: how many entries, and how many still open.
#{
  let boxes = data.section_order.map(name => {
    let section = data.sections.at(name)
    let items = section.items
    let open = items.filter(i => "status" in i and i.status not in settled).len()
    let tracked = items.any(i => "status" in i)
    box(
      width: 100%,
      inset: (x: 8pt, y: 7pt),
      radius: 3pt,
      stroke: 0.6pt + luma(215),
      stack(
        spacing: 0.4em,
        text(size: 8pt, weight: "bold", fill: accent, upper(section.title)),
        text(size: 16pt, weight: "bold", str(items.len())),
        text(size: 8pt, fill: muted, if tracked [#open open] else [logged]),
      ),
    )
  })
  block(below: 1.2em, grid(columns: (1fr,) * boxes.len(), column-gutter: 8pt, ..boxes))
}

#if "notes" in f { md(doc, "notes") }

#let small(body) = text(size: 8.5pt, body)
#let opt(value) = if value == none { [] } else { value }

// Related IDs and references, small and muted, under an entry.
#let links(item) = {
  let related = item.at("related", default: ())
  let parts = ()
  if related.len() > 0 { parts.push([Related: #related.join(", ")]) }
  let r = refs-of(item)
  if r != none { parts.push(r) }
  if parts.len() > 0 { block(above: 0.4em, text(size: 8pt, fill: muted, parts.join([ · ]))) }
}

// The entry in bold, an optional text field below it, then its links.
#let what(extra: none) = item => {
  strong(field(item, "title"))
  if extra != none and field(item, extra) != none {
    block(above: 0.45em, small(md(item, extra)))
  }
  links(item)
}
#let id(item) = text(size: 8.5pt, weight: "bold", field(item, "key"))
#let plain(name) = item => small(opt(field(item, name)))
#let level(name) = item => pill(field(item, name))
#let prose(name) = item => small(opt(md(item, name)))
#let labelled(name, label) = item => if field(item, name) != none {
  block(below: 0.5em, small[#text(fill: muted)[#label:] #md(item, name)])
}
#let stacked(..cells) = item => cells.pos().map(c => c(item)).join()

#let tables = (
  risks: (
    ("ID", 0.55in, id),
    ("Risk", 3fr, what(extra: "description")),
    ("Likelihood", 0.75in, level("likelihood")),
    ("Severity", 0.7in, level("severity")),
    ("Owner", 0.85in, plain("owner")),
    ("Status", 0.85in, level("status")),
    ("Mitigation", 2.4fr, prose("mitigation")),
    ("Raised", 0.75in, plain("raised")),
  ),
  assumptions: (
    ("ID", 0.55in, id),
    ("Assumption", 2.4fr, what()),
    ("Basis", 1.3fr, plain("basis")),
    ("Impact if wrong", 1.6fr, prose("impact")),
    ("How to validate", 1.6fr, prose("validation")),
    ("Status", 0.85in, level("status")),
  ),
  issues: (
    ("ID", 0.55in, id),
    ("Issue", 3fr, what(extra: "description")),
    ("Severity", 0.7in, level("severity")),
    ("Owner", 0.85in, plain("owner")),
    ("Status", 0.85in, level("status")),
    ("Action", 2.4fr, stacked(labelled("action", "Next"), labelled("resolution", "Resolved"))),
    ("Due", 0.75in, plain("due")),
  ),
  dependencies: (
    ("ID", 0.55in, id),
    ("Dependency", 2.6fr, what(extra: "notes")),
    ("Depends on", 1.1in, plain("on")),
    ("Impact if late", 2fr, prose("impact")),
    ("Owner", 0.85in, plain("owner")),
    ("Status", 0.8in, level("status")),
    ("Needed by", 0.75in, plain("needed_by")),
  ),
  decisions: (
    ("ID", 0.65in, id),
    ("Decided", 0.75in, plain("date")),
    ("Decision", 3fr, what(extra: "notes")),
    ("Rationale", 2.2fr, prose("rationale")),
    ("Decided by", 1.1in, plain("decided_by")),
  ),
)

#for name in data.section_order {
  let section = data.sections.at(name)
  heading(level: 1)[#section.title #text(weight: "regular", fill: muted)[(#section.items.len())]]
  if section.items.len() == 0 {
    text(fill: muted)[None logged.]
  } else if name in tables {
    record-table(section.items, tables.at(name))
  } else {
    for item in section.items [- #field(item, "title")]
  }
}

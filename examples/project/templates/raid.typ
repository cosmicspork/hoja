// RAID log: one table per kind, landscape.
#import "hoja.typ": md
#import "common.typ": *

#let data = json("data.json")
#let doc = data.document
#let f = doc.fields
#show: body => setup(data, body, flipped: true)

#title-block(data, subtitle: [
  RAID log · #f.at("project", default: "")
  #if "as_of" in f [ · as of #f.as_of]
])

#if "notes" in f { md(doc, "notes") }

// The entry's title in bold, its description below in small type.
#let what(item) = {
  strong(field(item, "title"))
  if field(item, "description") != none {
    block(above: 0.4em, text(size: 8.5pt, md(item, "description")))
  }
  let r = refs-of(item)
  if r != none { block(above: 0.3em, r) }
}
#let id(item) = text(size: 8.5pt, weight: "bold", field(item, "key"))
#let plain(name) = item => text(size: 9pt, field(item, name))
#let level(name) = item => pill(field(item, name))
#let prose(name) = item => text(size: 8.5pt, md(item, name))

#let tables = (
  risks: (
    ("ID", 0.55in, id),
    ("Risk", 2.6fr, what),
    ("Likelihood", 0.8in, level("likelihood")),
    ("Severity", 0.75in, level("severity")),
    ("Owner", 0.9in, plain("owner")),
    ("Status", 0.95in, level("status")),
    ("Mitigation", 2fr, prose("mitigation")),
    ("Review by", 0.75in, plain("review_by")),
  ),
  assumptions: (
    ("ID", 0.55in, id),
    ("Assumption", 3fr, what),
    ("Owner", 1in, plain("owner")),
    ("Status", 0.95in, level("status")),
    ("Validate by", 0.85in, plain("validate_by")),
  ),
  issues: (
    ("ID", 0.55in, id),
    ("Issue", 2.6fr, what),
    ("Severity", 0.75in, level("severity")),
    ("Owner", 0.9in, plain("owner")),
    ("Status", 0.95in, level("status")),
    ("Resolution", 2fr, prose("resolution")),
    ("Due", 0.75in, plain("due")),
  ),
  dependencies: (
    ("ID", 0.55in, id),
    ("Dependency", 2.6fr, what),
    ("Depends on", 1.2in, plain("on")),
    ("Owner", 0.9in, plain("owner")),
    ("Status", 0.95in, level("status")),
    ("Needed by", 0.8in, plain("needed_by")),
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

// Monthly status report: the summary, what changed, then the top risks and
// open issues selected from the RAID log.
#import "hoja.typ": md
#import "common.typ": *

#let data = json("data.json")
#let doc = data.document
#let f = doc.fields
#show: body => setup(data, body)

#title-block(data, subtitle: [Status · #f.at("period", default: "")])

#if "summary" in f { md(doc, "summary") }

#for name in ("done", "in_progress", "next") {
  let section = data.sections.at(name)
  if section.items.len() > 0 {
    heading(level: 1, section.title)
    for item in section.items [
      - #md(item, "text") #refs-of(item)#if "project" in item [ #text(size: 8.5pt, fill: muted)[\[#item.project\]]]
    ]
  }
}

#let raid = (
  risks: (
    ("ID", 0.55in, r => text(size: 8.5pt, weight: "bold", field(r, "key"))),
    ("Risk", 2fr, r => strong(field(r, "title"))),
    ("Severity", 0.75in, r => pill(field(r, "severity"))),
    ("Status", 0.85in, r => pill(field(r, "status"))),
    ("Mitigation", 2fr, r => text(size: 9pt, md(r, "mitigation"))),
  ),
  issues: (
    ("ID", 0.55in, i => text(size: 8.5pt, weight: "bold", field(i, "key"))),
    ("Issue", 2fr, i => strong(field(i, "title"))),
    ("Severity", 0.75in, i => pill(field(i, "severity"))),
    ("Owner", 1fr, i => field(i, "owner")),
    ("Due", 0.75in, i => field(i, "due")),
  ),
)
#for (name, columns) in raid {
  let section = data.sections.at(name, default: none)
  if section != none and section.items.len() > 0 {
    heading(level: 1, section.title)
    record-table(section.items, columns)
  }
}

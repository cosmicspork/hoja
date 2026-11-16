// Project charter: the person's prose sections, then milestones,
// stakeholders, the top risks from the RAID log, and a sign-off block.
#import "hoja.typ": md
#import "common.typ": *

#let data = json("data.json")
#let doc = data.document
#let f = doc.fields
#show: body => setup(data, body)

#title-block(data, subtitle: [Project charter])

#let facts = (("Project", "project"), ("Sponsor", "sponsor"), ("Project manager", "manager"))
#grid(
  columns: (auto, 1fr),
  column-gutter: 14pt,
  row-gutter: 6pt,
  ..facts.filter(p => p.at(1) in f).map(p => (text(fill: muted, p.at(0)), f.at(p.at(1)))).flatten(),
)

#let sections = (
  ("Purpose", "purpose"),
  ("Background", "background"),
  ("Objectives", "objectives"),
  ("In scope", "scope"),
  ("Out of scope", "out_of_scope"),
  ("Deliverables", "deliverables"),
)
#for (title, name) in sections {
  if name in f {
    heading(level: 1, title)
    md(doc, name)
  }
}

#let milestones = data.sections.milestones.items
#if milestones.len() > 0 {
  heading(level: 1)[Milestones]
  record-table(milestones, (
    ("Milestone", 1.3fr, m => strong(field(m, "name"))),
    ("Target", 0.9in, m => field(m, "target")),
    ("What is true when it's done", 2fr, m => text(size: 9pt, md(m, "description"))),
  ))
}

#for (title, name) in (("Success criteria", "success_criteria"), ("Constraints", "constraints"), ("Budget and resources", "budget")) {
  if name in f {
    heading(level: 1, title)
    md(doc, name)
  }
}

#let people = data.sections.stakeholders.items
#if people.len() > 0 {
  heading(level: 1)[Stakeholders]
  record-table(people, (
    ("Who", 1.2fr, p => strong(field(p, "name"))),
    ("Role", 1.2fr, p => field(p, "role")),
    ("Involvement", 0.95in, p => pill(field(p, "involvement"))),
    ("Notes", 2fr, p => text(size: 9pt, md(p, "notes"))),
  ))
}

#let risks = data.sections.risks.items
#if risks.len() > 0 {
  heading(level: 1)[Initial risks]
  text(size: 9pt, fill: muted)[From the RAID log, which holds every risk and its current status.]
  record-table(risks, (
    ("ID", 0.55in, r => text(size: 8.5pt, weight: "bold", field(r, "key"))),
    ("Risk", 2fr, r => strong(field(r, "title"))),
    ("Likelihood", 0.8in, r => pill(field(r, "likelihood"))),
    ("Severity", 0.75in, r => pill(field(r, "severity"))),
    ("Owner", 1fr, r => field(r, "owner")),
  ))
}

#v(1.5em)
#heading(level: 1)[Sign-off]
#block(breakable: false, grid(
  columns: (1fr, 1fr),
  column-gutter: 30pt,
  ..(("Sponsor", f.at("sponsor", default: none)), ("Project manager", f.at("manager", default: none))).map(((role, name)) => [
    #v(2.2em)
    #line(length: 100%, stroke: 0.5pt + muted)
    #text(size: 9pt)[#role#if name != none [: #name] #h(1fr) Date]
  ]),
))

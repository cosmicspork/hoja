// The shared look of the project documents. Each template imports this
// file, so accepting a change here returns every approved document that
// uses it to draft.

#let accent = rgb("#2f5d50")
#let muted = luma(95)
#let alarm = rgb("#b42318")

#let setup(data, body, flipped: false) = {
  let doc = data.document
  set document(title: doc.title, author: if data.owner != none { data.owner } else { () })
  set page(
    paper: "us-letter",
    flipped: flipped,
    margin: (x: 0.75in, y: 0.7in),
    footer: context {
      set text(size: 8pt, fill: muted)
      doc.title
      h(1fr)
      if not doc.approved { text(fill: alarm, weight: "bold")[DRAFT] + [ · ] }
      counter(page).display("1 of 1", both: true)
    },
  )
  set text(font: ("Liberation Sans", "Noto Sans", "Helvetica Neue", "Arial"), size: 10pt)
  set par(justify: false, leading: 0.62em, spacing: 0.9em)
  show heading.where(level: 1): set text(size: 13pt, fill: accent)
  show heading.where(level: 2): set text(size: 11pt)
  show heading: set block(above: 1.3em, below: 0.6em)
  show link: set text(fill: accent)
  body
}

// The title, then who and when: the approval date, or DRAFT and today.
#let title-block(data, subtitle: none) = {
  let doc = data.document
  block(below: 1em, {
    text(size: 20pt, weight: "bold", doc.title)
    linebreak()
    set text(fill: muted)
    if subtitle != none { subtitle + [ · ] }
    if data.owner != none { data.owner + [ · ] }
    if doc.approved and doc.approved_at != none [Approved #doc.approved_at.slice(0, 10)] else [
      #text(fill: alarm, weight: "bold")[DRAFT] · #data.date
    ]
  })
}

#let level-colors = (
  low: luma(110),
  medium: rgb("#9a5b13"),
  high: alarm,
  critical: rgb("#7a1010"),
  open: alarm,
  mitigating: rgb("#9a5b13"),
  in_progress: rgb("#9a5b13"),
  at_risk: rgb("#9a5b13"),
  waiting: luma(110),
  on_track: accent,
  unvalidated: luma(110),
  closed: accent,
  resolved: accent,
  met: accent,
  valid: accent,
  occurred: alarm,
  missed: alarm,
  invalid: alarm,
)

// An enum value as a small coloured label; "not set" when it's empty.
#let pill(value) = {
  if value == none { return text(size: 8.5pt, fill: muted, style: "italic")[not set] }
  let color = level-colors.at(value, default: muted)
  box(
    inset: (x: 4pt, y: 1.5pt),
    radius: 3pt,
    fill: color.lighten(88%),
    text(size: 8.5pt, weight: "semibold", fill: color, value.replace("_", " ")),
  )
}

// A field of a record or document, or none.
#let field(source, name) = source.at(name, default: none)

// A table with an accent rule under the header and hairlines between rows.
// `columns` holds (header, width, cell) where cell(item) gives the content.
#let record-table(items, columns) = table(
  columns: columns.map(c => c.at(1)),
  stroke: (x, y) => if y == 0 { (bottom: 0.8pt + accent) } else { (bottom: 0.4pt + luma(215)) },
  inset: (x: 5pt, y: 5pt),
  align: left + top,
  table.header(..columns.map(c => text(size: 8.5pt, weight: "bold", fill: accent, c.at(0)))),
  ..items.map(item => columns.map(c => (c.at(2))(item))).flatten(),
)

#let refs-of(item) = {
  let list = item.at("refs", default: ())
  if list.len() > 0 { text(size: 8pt, fill: muted, list.map(r => raw(r)).join(", ")) }
}

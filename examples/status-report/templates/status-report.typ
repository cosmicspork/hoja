// Monthly status report. hoja writes data.json next to this file.
#let data = json("data.json")
#let doc = data.document
#let fields = doc.fields

#set document(title: doc.title, author: if data.owner != none { data.owner } else { () })
#set page(paper: "us-letter", margin: (x: 0.9in, y: 0.8in), numbering: "1")
#set text(font: ("Liberation Sans", "Noto Sans"), size: 10.5pt)
#set par(justify: false, leading: 0.6em)

#let refs(item) = {
  let list = item.at("refs", default: ())
  if list.len() > 0 {
    text(size: 9pt, fill: luma(90))[ (#list.map(r => raw(r)).join(", "))]
  }
}

#text(size: 18pt, weight: "bold")[#doc.title]
#v(-0.4em)
#text(fill: luma(80))[
  #fields.at("period", default: "")
  #if data.owner != none [ · #data.owner]
  #if not doc.approved [ · #text(fill: rgb("#b42318"), weight: "bold")[DRAFT]]
]

#if "summary" in fields {
  v(0.6em)
  fields.summary
}

#for name in data.section_order {
  let section = data.sections.at(name)
  if section.items.len() > 0 {
    v(0.8em)
    text(size: 12.5pt, weight: "bold")[#section.title]
    v(0.2em)
    for item in section.items [
      - #item.text#refs(item)#if "project" in item [ #text(size: 9pt, fill: luma(90))[\[#item.project\]]]
    ]
  }
}

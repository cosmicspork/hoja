// hoja's helpers for templates. hoja writes this file into every render
// root, so a template can `#import "hoja.typ": md` without keeping a copy.
//
// Text fields are Markdown. hoja parses them and puts the tree next to the
// raw string (under `_md`), so templates render lists, emphasis and
// sub-headings without evaluating any text as Typst.

#let _inline(node) = {
  if type(node) == str { return node }
  let kids = node.at("c", default: ()).map(_inline).join()
  if node.t == "em" { emph(kids) }
  else if node.t == "strong" { strong(kids) }
  else if node.t == "strike" { strike(kids) }
  else if node.t == "code" { raw(node.text) }
  else if node.t == "link" { link(node.url, kids) }
  else if node.t == "br" { linebreak() }
  else { kids }
}

// One block, or a list of blocks separated by paragraph breaks.
#let _render(node, offset) = {
  if type(node) == array { return node.map(n => _render(n, offset)).join(parbreak()) }
  if node.t == "p" { node.c.map(_inline).join() }
  else if node.t == "h" {
    heading(level: calc.min(node.level + offset, 6), outlined: false, node.c.map(_inline).join())
  }
  else if node.t == "list" {
    let items = node.items.map(item => _render(item, offset))
    if node.ordered { enum(start: node.start, ..items) } else { list(..items) }
  }
  else if node.t == "quote" { quote(block: true, _render(node.c, offset)) }
  else if node.t == "code" { raw(node.text, block: true) }
  else if node.t == "hr" { line(length: 100%, stroke: 0.5pt + luma(160)) }
}

/// A Markdown text field as content: `md(data.document, "summary")` for a
/// document field, `md(item, "description")` for a record's. Markdown
/// headings start one level below the template's own (`heading-offset`).
/// Returns none when the field is empty.
#let md(source, field, heading-offset: 1) = {
  let tree = source.at("_md", default: (:)).at(field, default: none)
  if tree != none { return _render(tree, heading-offset) }
  source.at("fields", default: source).at(field, default: none)
}

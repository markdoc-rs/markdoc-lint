//! Tags that do not close, closes with nothing to close, what the parser
//! could not read, and the one typo that looks like an anchor and is not.

use crate::rules::{Rule, RuleInfo};
use crate::{Config, Corpus, Diagnostic, Document, Severity, Span};
use markdoc::ast::Node;
use markdoc::ext::{Occurrence, OccurrenceKind};
use markdoc::types::{Scalar, ValidationError, ValidationLevel};
use std::collections::HashSet;

pub struct Syntax;

impl Rule for Syntax {
    fn describe(&self) -> RuleInfo {
        RuleInfo {
            name: "syntax",
            summary: "Tags that never close, closes with nothing to close, and the typo behind both.",
            diagnostics: &[
                "unclosed-tag",
                "unmatched-closing-tag",
                "malformed-anchor",
                "unparsable",
                "syntax",
            ],
            default_severity: Severity::Error,
            // Always on: this is Markdoc's own grammar, not a convention.
            // `malformed-anchor` needs `[anchors] declare_tag` to recognise
            // the tag it gives better advice about, and says the generic
            // thing without it.
            configured_by: None,
        }
    }

    fn check(&self, doc: &Document<'_>, corpus: &Corpus, out: &mut Vec<Diagnostic>) {
        check(doc, &corpus.config, out);
    }
}

/// The errors the parser and the tokenizer record on the tree: what this
/// rule reports, and what the schema rule must not report a second time
/// (`validate` re-emits every node's errors).
pub const FROM_THE_PARSER: &[&str] = &[
    "missing-closing",
    "missing-opening",
    "parse-error",
    "duplicate-attribute",
    "no-inline-annotations",
    "frontmatter-invalid",
    "fence-tag-error",
    "href-format-invalid",
    "table-syntax",
];

pub fn check(doc: &Document<'_>, config: &Config, out: &mut Vec<Diagnostic>) {
    let rel = doc.rel;

    // `{% tag="docking" %}` -- an anchor written with an `=` and no space.
    //
    // Found first, because it has to suppress what the parser says about
    // it. Upstream reads `{% key=value %}` as an annotation: attributes for
    // the block it ends, or, on a line of its own, for nothing ("Can't
    // apply inline annotations to 'document'"). Either way no anchor is
    // declared, and every reference aimed at it resolves to nothing, while
    // the page renders as if all were well.
    //
    // Only the configured anchor tag is checked, because only there is the
    // intent unambiguous: `{% key=value %}` with any other key is an
    // annotation somebody may mean.
    //
    // Written self-closing, `{% tag="docking" /%}`, it is not even that: an
    // annotation cannot close, and the grammar stops at the `/`. Same typo,
    // same advice.
    let mut typos: HashSet<usize> = HashSet::new();
    if let Some(declare) = &config.anchors.declare_tag {
        for o in doc.occurrences.iter().filter(|o| !o.in_fence) {
            let value = match &o.kind {
                OccurrenceKind::Annotation { attributes } => match attributes.get(declare) {
                    Some(value) => value.clone(),
                    None => continue,
                },
                OccurrenceKind::Error { .. } => {
                    match keyed_like(doc.source.get(o.span.clone()).unwrap_or(""), declare) {
                        Some(value) => Scalar::String(value),
                        None => continue,
                    }
                }
                _ => continue,
            };
            typos.insert(o.span.start);
            out.push(malformed_anchor(
                rel,
                declare,
                &value,
                Some(Span::of_occurrence(o)),
            ));
        }
    }

    // Tags left open and closes with nothing to close, from the tokenizer's
    // own pairing. The tree cannot say these well: a tag left open inside a
    // paragraph or a table cell puts the errors on the Markdown nodes
    // around it ("Node 'inline' is missing closing"), and none of them
    // names the tag.
    let (unclosed, unmatched) = unpaired(doc.occurrences);
    for &i in &unclosed {
        let o = &doc.occurrences[i];
        let name = name_of(o);
        out.push(
            Diagnostic::new(
                rel,
                Severity::Error,
                "unclosed-tag",
                format!(
                    "`{{% {name} %}}` is never closed, so what follows it became its \
                     content; add `{{% /{name} %}}`."
                ),
            )
            .at(Some(Span::of_occurrence(o))),
        );
    }
    for &i in &unmatched {
        let o = &doc.occurrences[i];
        out.push(unmatched_closing(
            rel,
            name_of(o),
            Some(Span::of_occurrence(o)),
        ));
    }
    let pairing_reported = !unclosed.is_empty() || !unmatched.is_empty();

    // The rest of what the parser recorded, on whichever node it put it.
    every_error(doc.tree, &mut |node, err| {
        let span = Span::of_error(err).or_else(|| Span::of_node(node));
        let diagnostic = match err.id.as_str() {
            // Said above, once per tag rather than once per Markdown node
            // around it. Only when the tokenizer's pairing found nothing
            // (it always should) does the parser's own report stand.
            "missing-closing" | "missing-opening" if pairing_reported => return,
            "missing-closing" => match node.tag.as_deref() {
                Some(name) => Diagnostic::new(
                    rel,
                    Severity::Error,
                    "unclosed-tag",
                    format!("`{{% {name} %}}` is never closed; add `{{% /{name} %}}`."),
                ),
                None => Diagnostic::new(rel, Severity::Error, "unclosed-tag", &err.message),
            },
            "missing-opening" => match node.tag.as_deref() {
                Some(name) => unmatched_closing(rel, name, None),
                None => {
                    Diagnostic::new(rel, Severity::Error, "unmatched-closing-tag", &err.message)
                }
            },
            // The anchor typo, said above: on a line of its own, or
            // written self-closing.
            "no-inline-annotations" | "parse-error"
                if span.is_some_and(|s| typos.contains(&s.start_offset)) =>
            {
                return;
            }
            // Frontmatter that is not YAML: the parser keeps it as text, and
            // every rule that reads it as data goes quiet, so this is what
            // the lint has always called a document it cannot read.
            "frontmatter-invalid" => Diagnostic::new(
                rel,
                Severity::Error,
                "unparsable",
                format!("The frontmatter is not YAML: {}", err.message),
            ),
            _ => Diagnostic::new(rel, severity(&err.level), "syntax", &err.message),
        };
        out.push(diagnostic.at(span));
    });
}

/// Every error on every node, the root's included, wherever it is.
fn every_error<'a>(node: &'a Node, f: &mut impl FnMut(&'a Node, &'a ValidationError)) {
    for err in &node.errors {
        f(node, err);
    }
    for child in &node.children {
        every_error(child, f);
    }
    for slot in node.slots.values() {
        every_error(slot, f);
    }
}

fn severity(level: &ValidationLevel) -> Severity {
    match level {
        ValidationLevel::Critical | ValidationLevel::Error => Severity::Error,
        ValidationLevel::Warning => Severity::Warning,
        ValidationLevel::Debug | ValidationLevel::Info => Severity::Hint,
        // A hand-made token's own level: reported as the error upstream
        // defaults to.
        ValidationLevel::Other(_) => Severity::Error,
    }
}

fn name_of(o: &Occurrence) -> &str {
    match &o.kind {
        OccurrenceKind::Open { name, .. }
        | OccurrenceKind::SelfClosing { name, .. }
        | OccurrenceKind::Close { name } => name,
        _ => "",
    }
}

/// The opening tags left open and the closing tags that closed nothing, as
/// the parser paired them -- less the pairs that are only collateral.
///
/// The parser closes a tag only when it is the innermost one open. So in
/// `{% table %} … {% callout %}never closed … {% /table %}` the
/// `{% /table %}` arrives while `callout` is open, closes nothing, and
/// leaves `table` open as well: three unpaired tags and one fault. A
/// closing tag that has an unpaired opening tag of its own name before it,
/// with something left open in between, is that collateral, and the two
/// are not reported: closing the culprit pairs them.
fn unpaired(occurrences: &[Occurrence]) -> (Vec<usize>, Vec<usize>) {
    let is = |i: &usize, open: bool| {
        let o = &occurrences[*i];
        o.partner.is_none()
            && match o.kind {
                OccurrenceKind::Open { .. } => open,
                OccurrenceKind::Close { .. } => !open,
                _ => false,
            }
    };
    let opens: Vec<usize> = (0..occurrences.len()).filter(|i| is(i, true)).collect();
    let closes: Vec<usize> = (0..occurrences.len()).filter(|i| is(i, false)).collect();
    let mut repaired = vec![false; occurrences.len()];
    for &c in &closes {
        let name = name_of(&occurrences[c]);
        let Some(&o) = opens
            .iter()
            .rev()
            .find(|&&o| o < c && !repaired[o] && name_of(&occurrences[o]) == name)
        else {
            continue;
        };
        let culprit = opens.iter().any(|&x| x > o && x < c && !repaired[x]);
        if culprit {
            repaired[o] = true;
            repaired[c] = true;
        }
    }
    let keep = |v: Vec<usize>| v.into_iter().filter(|&i| !repaired[i]).collect();
    (keep(opens), keep(closes))
}

fn unmatched_closing(rel: &str, name: &str, span: Option<Span>) -> Diagnostic {
    Diagnostic::new(
        rel,
        Severity::Error,
        "unmatched-closing-tag",
        format!(
            "`{{% /{name} %}}` closes a tag that is not open here. Either the opening \
             `{{% {name} %}}` is missing, or something between them is closed in the wrong \
             order."
        ),
    )
    .at(span)
}

/// The id in `{% key="id" /%}`, when `key` is the anchor tag: a tag the
/// grammar could not read that starts like the anchor typo.
fn keyed_like(tag: &str, declare: &str) -> Option<String> {
    let body = tag.strip_prefix("{%")?.trim_start();
    let rest = body.strip_prefix(declare)?.trim_start().strip_prefix('=')?;
    let rest = rest.trim_start().strip_prefix('"')?;
    Some(rest[..rest.find('"')?].to_string())
}

/// The report for an anchor written as an annotation.
fn malformed_anchor(rel: &str, declare: &str, value: &Scalar, span: Option<Span>) -> Diagnostic {
    let id = match value {
        Scalar::String(s) if !s.trim().is_empty() => s.trim(),
        _ => "some_id",
    };
    Diagnostic::new(
        rel,
        Severity::Error,
        "malformed-anchor",
        format!(
            "`{{% {declare}=… %}}` is an annotation, not an anchor: it gives the block it \
             ends an attribute `{declare}` that nothing reads, and declares no anchor."
        ),
    )
    .at(span)
    .help(format!(
        "An anchor is self-closing and takes its id as an unkeyed value: write \
         `{{% {declare} \"{id}\" /%}}`, with a space and no `=`. As written, every \
         reference aimed at it resolves to nothing."
    ))
}

#[cfg(test)]
mod tests {
    use crate::{Config, Corpus, lint_document};

    fn anchored() -> Corpus {
        let config = Config::parse("[anchors]\ndeclare_tag = \"tag\"\n").unwrap();
        Corpus {
            config,
            ..Default::default()
        }
    }

    fn rules_for(corpus: &Corpus, src: &str) -> Vec<String> {
        lint_document("a.md", src, corpus)
            .diagnostics
            .into_iter()
            .map(|d| d.rule.to_string())
            .collect()
    }

    #[test]
    fn the_anchor_typo_is_reported_once_and_with_usable_advice() {
        let c = anchored();
        let linted = lint_document("a.md", "# Docking {% tag=\"docking\" %}\n\nProse.\n", &c);
        let rules: Vec<&str> = linted.diagnostics.iter().map(|d| d.rule.as_ref()).collect();
        // Once: not again as the heading's undefined attribute `tag`, which
        // is what the annotation gives it.
        assert_eq!(rules, vec!["malformed-anchor"], "{:?}", linted.diagnostics);
        let d = &linted.diagnostics[0];
        assert!(
            d.help
                .as_ref()
                .unwrap()
                .contains("`{% tag \"docking\" /%}`"),
            "{d:?}"
        );
        // At the annotation itself.
        let span = d.span.unwrap();
        assert_eq!((span.start_line, span.start_char), (1, 10));
    }

    #[test]
    fn an_anchor_typo_on_a_line_of_its_own_is_reported_once() {
        // Upstream has nothing to apply it to and says "Can't apply inline
        // annotations to 'document'", which is true and no help.
        let c = anchored();
        assert_eq!(
            rules_for(&c, "# Docking\n\n{% tag=\"docking\" %}\n\nProse.\n"),
            vec!["malformed-anchor".to_string()]
        );
    }

    #[test]
    fn an_anchor_typo_written_self_closing_is_the_same_typo() {
        // Not even an annotation: the grammar stops at the `/`.
        let c = anchored();
        let linted = lint_document("a.md", "# Docking {% tag=\"docking\" /%}\n", &c);
        let rules: Vec<&str> = linted.diagnostics.iter().map(|d| d.rule.as_ref()).collect();
        assert_eq!(rules, vec!["malformed-anchor"], "{:?}", linted.diagnostics);
        assert!(
            linted.diagnostics[0]
                .help
                .as_ref()
                .unwrap()
                .contains("`{% tag \"docking\" /%}`")
        );
        // Without the anchor tag configured it is what the grammar says.
        let linted = lint_document(
            "a.md",
            "# Docking {% tag=\"docking\" /%}\n",
            &Corpus::empty(),
        );
        let rules: Vec<&str> = linted.diagnostics.iter().map(|d| d.rule.as_ref()).collect();
        assert_eq!(rules, vec!["syntax"], "{:?}", linted.diagnostics);
    }

    #[test]
    fn an_anchor_typo_in_a_list_table_is_reported_once() {
        let c = anchored();
        let src = "{% table %}\n* Station\n* Notes\n---\n* Docking {% tag=\"docking\" %}\n* Prose.\n{% /table %}\n";
        let linted = lint_document("a.md", src, &c);
        let rules: Vec<&str> = linted.diagnostics.iter().map(|d| d.rule.as_ref()).collect();
        assert_eq!(rules, vec!["malformed-anchor"], "{:?}", linted.diagnostics);
        // At the annotation, in its cell: every node of a table has a
        // position of its own now, so there is no borrowing the table's.
        let span = linted.diagnostics[0].span.as_ref().expect("placed");
        assert_eq!((span.start_line, span.start_char), (5, 10));
        // One report per typo.
        let two = "{% table %}\n* Station\n* Notes\n---\n* Docking {% tag=\"docking\" %}\n* Prose.\n---\n* Charging {% tag=\"charging\" %}\n* More.\n{% /table %}\n";
        assert_eq!(
            rules_for(&c, two),
            vec![
                "malformed-anchor".to_string(),
                "malformed-anchor".to_string()
            ]
        );
    }

    #[test]
    fn a_tag_left_open_beside_an_anchor_typo_in_a_table_is_still_reported() {
        // The tag left open in a cell is named, though the parser's errors
        // are all on the Markdown around it; and neither the table it broke
        // nor the blocks it swallowed are reported as faults of their own.
        // (`schema` is upstream's other true thing to say about it: a
        // callout written inside a cell's text is inline, and a callout is
        // a block.)
        let c = anchored();
        let src = "{% table %}\n* Station\n* Notes\n---\n* Docking {% tag=\"docking\" %}\n* Prose.\n---\n* {% callout %}never closed\n* More.\n{% /table %}\n";
        let linted = lint_document("a.md", src, &c);
        let rules: Vec<&str> = linted.diagnostics.iter().map(|d| d.rule.as_ref()).collect();
        assert_eq!(
            rules,
            vec!["malformed-anchor", "schema", "unclosed-tag"],
            "{:?}",
            linted.diagnostics
        );
        let unclosed = linted
            .diagnostics
            .iter()
            .find(|d| d.rule == "unclosed-tag")
            .unwrap();
        assert!(
            unclosed.message.contains("`{% callout %}`"),
            "{}",
            unclosed.message
        );
        assert_eq!(unclosed.span.unwrap().start_line, 8);
    }

    #[test]
    fn an_anchor_typo_that_swallowed_nothing_gets_the_anchor_advice() {
        let c = anchored();
        let src = "{% table %}\n* Station\n* Notes\n---\n* Docking\n* Prose {% tag=\"docking\" %}\n---\n* x\n* y\n{% /table %}\n";
        assert_eq!(rules_for(&c, src), vec!["malformed-anchor".to_string()]);
        assert_eq!(
            rules_for(&c, "Prose.\n\n# Docking {% tag=\"docking\" %}\n"),
            vec!["malformed-anchor".to_string()]
        );
    }

    #[test]
    fn a_correct_anchor_says_nothing() {
        let c = anchored();
        assert!(rules_for(&c, "# Docking {% tag \"docking\" /%}\n\nProse.\n").is_empty());
    }

    #[test]
    fn an_ordinary_unclosed_tag_still_gets_the_generic_report() {
        let c = anchored();
        assert!(rules_for(&c, "{% callout %}\nbody\n").contains(&"unclosed-tag".to_string()));
    }

    #[test]
    fn a_tag_left_open_inside_a_paragraph_is_named() {
        // Upstream's errors here are about the paragraph and its inline
        // content; the tag is what the writer has to close.
        let linted = lint_document("a.md", "Text {% callout %} more.\n", &Corpus::empty());
        let unclosed: Vec<_> = linted
            .diagnostics
            .iter()
            .filter(|d| d.rule == "unclosed-tag")
            .collect();
        assert_eq!(unclosed.len(), 1, "{:?}", linted.diagnostics);
        assert!(unclosed[0].message.contains("`{% callout %}`"));
        assert_eq!(unclosed[0].span.unwrap().start_char, 5);
    }

    #[test]
    fn of_tags_closed_out_of_order_the_one_left_open_is_reported() {
        // `{% /a %}` arrives while `b` is open: the parser pairs nothing,
        // and the fault is `b`.
        let linted = lint_document("a.md", "{% a %}\n\n{% b %}\n\n{% /a %}\n", &Corpus::empty());
        let syntax: Vec<(&str, usize)> = linted
            .diagnostics
            .iter()
            .filter(|d| d.rule == "unclosed-tag" || d.rule == "unmatched-closing-tag")
            .map(|d| (d.rule.as_ref(), d.span.unwrap().start_line))
            .collect();
        assert_eq!(
            syntax,
            vec![("unclosed-tag", 3)],
            "{:?}",
            linted.diagnostics
        );
    }

    #[test]
    fn a_parser_error_is_not_reported_twice() {
        // `validate` re-emits `node.errors`, so without filtering every
        // syntax fault appeared once as itself and once as `schema`; and the
        // stray closer is a node of its own, which must not be validated as
        // a `callout` either.
        let c = anchored();
        assert_eq!(
            rules_for(&c, "Text.\n\n{% /callout %}\n"),
            vec!["unmatched-closing-tag".to_string()],
            "one fault, one line of output"
        );
    }

    #[test]
    fn what_the_grammar_cannot_read_is_a_syntax_error() {
        let linted = lint_document("a.md", "Text {% $robot. %} more.\n", &Corpus::empty());
        let rules: Vec<&str> = linted.diagnostics.iter().map(|d| d.rule.as_ref()).collect();
        assert_eq!(rules, vec!["syntax"], "{:?}", linted.diagnostics);
    }

    #[test]
    fn frontmatter_that_is_not_yaml_is_unparsable() {
        // D22: what the lint has always said of a file whose YAML fails.
        let linted = lint_document(
            "a.md",
            "---\ntitle: [unclosed\n---\n\n# T\n",
            &Corpus::empty(),
        );
        let rules: Vec<&str> = linted.diagnostics.iter().map(|d| d.rule.as_ref()).collect();
        assert_eq!(rules, vec!["unparsable"], "{:?}", linted.diagnostics);
    }

    #[test]
    fn with_no_anchor_tag_configured_the_typo_rule_is_silent() {
        // Upstream default: nothing has said that `{% tag %}` is special.
        // What remains is what upstream says: the heading has an attribute
        // its schema does not declare.
        let c = Corpus::empty();
        let rules = rules_for(&c, "# Docking {% tag=\"docking\" %}\n\nProse.\n");
        assert!(
            !rules.contains(&"malformed-anchor".to_string()),
            "{rules:?}"
        );
        assert_eq!(rules, vec!["undefined-attribute".to_string()], "{rules:?}");
    }
}

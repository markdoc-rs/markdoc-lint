//! Tags and attributes against the schema the renderers use.
//!
//! This is a thin wrapper over `markdoc`'s `validate`, and that is the
//! whole point of it: the schema it checks against is the same set of tag
//! definitions a renderer transforms with. A second list of what a tag may
//! take would drift, and the drift would show up as a page that lints clean
//! and renders wrong.

use crate::rules::syntax::FROM_THE_PARSER;
use crate::rules::{Rule, RuleInfo};
use crate::{Corpus, Diagnostic, Document, Severity, Span, is_phantom};
use markdoc::ast::Node;
use markdoc::types::{Span as SourceSpan, ValidationLevel};
use serde::Deserialize;
use std::borrow::Cow;
use std::collections::HashSet;

/// Where this corpus declares the tags that are its own.
///
/// `markdoc` ships generic Markdoc. A corpus with a vocabulary of its own
/// says so in an overlay file, and every tool that reads the corpus must
/// load the same one — a tool that does not sees those tags as undeclared,
/// which is what `undefined-tag` reports.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct SchemaConfig {
    /// Corpus-relative path to a tag overlay, e.g.
    /// `.tooling/schemas/tags.overlay.toml`. Absent means the corpus uses
    /// only the built-in tags.
    pub overlay: Option<String>,
}

pub struct Schema;

impl Rule for Schema {
    fn describe(&self) -> RuleInfo {
        RuleInfo {
            name: "schema",
            summary: "Tags, their attributes and their children against the renderers' schema.",
            diagnostics: &[
                "missing-attribute",
                "invalid-attribute-type",
                "invalid-child",
                "undefined-attribute",
                "undefined-tag",
                "schema",
            ],
            default_severity: Severity::Error,
            // Always on. `[schema] overlay` extends the tag set it checks
            // against; it does not decide whether the rule runs, because a
            // corpus with no overlay still has Markdoc's own tags to get
            // wrong.
            configured_by: None,
        }
    }

    fn check(&self, doc: &Document<'_>, corpus: &Corpus, out: &mut Vec<Diagnostic>) {
        check(doc, corpus, out);
    }
}

pub fn check(doc: &Document<'_>, corpus: &Corpus, out: &mut Vec<Diagnostic>) {
    let rel = doc.rel;
    let tree = validated_tree(doc.tree, corpus.config.anchors.declare_tag.as_deref());

    // `validate` is `validate_tree` over the config merged with upstream's
    // defaults, which the corpus holds merged once.
    let errors = match markdoc::validator::validate_tree(&tree, corpus.merged_markdoc()) {
        Ok(errors) => errors,
        Err(e) => {
            out.push(
                Diagnostic::new(
                    rel,
                    Severity::Error,
                    "schema",
                    markdoc::thrown(&e).to_string(),
                )
                .at(Some(Span::line(1))),
            );
            return;
        }
    };

    // A node the parser left open swallows what follows it, and the
    // validator then says so of every block it swallowed: "Can't nest
    // 'paragraph' in 'inline'". `syntax` reports the tag left open.
    let left_open = located(&tree, &|n| {
        n.errors.iter().any(|e| e.id == "missing-closing")
    });
    // Upstream's `partial` looks its file up in `config.partials` ("Partial
    // `x` not found"), and the lint has no partials to give it: whether the
    // file exists is the `partials` rule's question, answered against the
    // corpus index with the corpus's own advice, and only where there is an
    // index to answer it.
    let partials = located(&tree, &|n| n.tag.as_deref() == Some("partial"));
    let mut tag_names: Option<Vec<String>> = None;
    for e in errors {
        let id = e.error.id.as_str();
        if FROM_THE_PARSER.contains(&id) {
            continue;
        }
        let at = e.span.as_ref().map(key);
        let within = |set: &HashSet<(usize, usize)>| at.is_some_and(|k| set.contains(&k));
        if (id == "child-invalid" && within(&left_open))
            || (id == "attribute-value-invalid" && within(&partials))
        {
            continue;
        }
        let span = Span::of_error(&e.error).or_else(|| e.span.as_ref().map(Span::from_span));
        if id == "tag-undefined" {
            let names = tag_names.get_or_insert_with(|| declared(corpus));
            out.push(undefined_tag(rel, &e.error.message, names).at(span));
            continue;
        }
        out.push(
            Diagnostic::new(rel, level(&e.error.level), rule_for(id), e.error.message).at(span),
        );
    }
}

/// The tree as the validator should see it: without the nodes `syntax`
/// already reported as something else.
///
/// - A stray closing tag is a phantom node of its own, with no attributes:
///   validated, a stray `{% /callout %}` would also be a callout missing
///   its required attributes.
/// - The anchor typo `{% tag="x" %}` gives the block it ends an attribute
///   `tag`, which its schema does not declare; `syntax` reports the typo.
fn validated_tree<'a>(tree: &'a Node, declare: Option<&str>) -> Cow<'a, Node> {
    let typo = |n: &Node| {
        declare.is_some_and(|d| {
            n.annotations
                .iter()
                .any(|a| a.attr_type == "attribute" && a.name == d)
        })
    };
    if !any(tree, &|n| is_phantom(n) || typo(n)) {
        return Cow::Borrowed(tree);
    }
    fn prune(node: &mut Node, declare: Option<&str>) {
        node.children.retain(|c| !is_phantom(c));
        if let Some(d) = declare
            && node
                .annotations
                .iter()
                .any(|a| a.attr_type == "attribute" && a.name == d)
        {
            node.attributes.shift_remove(d);
        }
        for child in &mut node.children {
            prune(child, declare);
        }
        for slot in node.slots.values_mut() {
            prune(slot, declare);
        }
    }
    let mut copy = tree.clone();
    prune(&mut copy, declare);
    Cow::Owned(copy)
}

fn any(node: &Node, f: &impl Fn(&Node) -> bool) -> bool {
    f(node) || node.children.iter().any(|c| any(c, f)) || node.slots.values().any(|s| any(s, f))
}

/// Where the nodes `want` picks are, as the validator's errors place them.
fn located(tree: &Node, want: &impl Fn(&Node) -> bool) -> HashSet<(usize, usize)> {
    let mut out = HashSet::new();
    let mut stack = vec![tree];
    while let Some(node) = stack.pop() {
        if want(node)
            && let Some(l) = &node.span
        {
            out.insert(key(l));
        }
        stack.extend(node.children.iter());
        stack.extend(node.slots.values());
    }
    out
}

fn key(l: &SourceSpan) -> (usize, usize) {
    (l.start.offset, l.end.offset)
}

fn level(l: &ValidationLevel) -> Severity {
    match l {
        ValidationLevel::Critical | ValidationLevel::Error => Severity::Error,
        ValidationLevel::Warning => Severity::Warning,
        ValidationLevel::Debug | ValidationLevel::Info => Severity::Hint,
        // Any other value is a schema's own `errorLevel` (upstream writes
        // `errorLevel || 'error'`): reported as the error it defaults to.
        ValidationLevel::Other(_) => Severity::Error,
    }
}

/// The lint's names for upstream's ids. A rule name is a thing people grep
/// for and put in an ignore list, so it outlives the code that emits it and
/// may not be renamed for tidiness: these are the names the validator this
/// replaces used, and every other id is `schema`.
fn rule_for(id: &str) -> &'static str {
    match id {
        "attribute-missing-required" => "missing-attribute",
        "attribute-type-invalid" => "invalid-attribute-type",
        "child-invalid" => "invalid-child",
        "attribute-undefined" => "undefined-attribute",
        "tag-undefined" => "undefined-tag",
        _ => "schema",
    }
}

/// Every tag name the merged config declares: for the near-miss advice,
/// and for what the vocabulary report calls "in schema".
pub(crate) fn declared(corpus: &Corpus) -> Vec<String> {
    markdoc::merge_config(&corpus.markdoc)
        .tags
        .into_iter()
        .filter_map(|(name, schema)| schema.map(|_| name))
        .collect()
}

/// A tag the schema does not define.
///
/// Upstream says so at its own level, critical; this says it as a warning,
/// as the lint always has. A corpus adopting the rule has a backlog, and the
/// fix is to declare the tag -- an edit to a schema file, not to the
/// thousands of pages using it. Until then the renderers render only its
/// content: its framing, and every attribute, are dropped.
///
/// It really happened. `{% notice %}` and `{% image %}` were written 2 808
/// times between them, were in no schema, and rendered as bare text in the
/// PDF while the website styled them.
fn undefined_tag(rel: &str, upstream: &str, declared: &[String]) -> Diagnostic {
    let Some(name) = upstream
        .strip_prefix("Undefined tag: '")
        .and_then(|s| s.strip_suffix('\''))
    else {
        return Diagnostic::new(rel, Severity::Warning, "undefined-tag", upstream);
    };
    let mut message = format!("Tag '{name}' is not defined in the schema");
    if let Some(near) = nearest(name, declared.iter()) {
        message.push_str(&format!(" (did you mean '{near}'?)"));
    }
    message.push_str(
        " — nothing validates its attributes, and the renderers render only its \
         content",
    );
    Diagnostic::new(rel, Severity::Warning, "undefined-tag", message)
}

/// The closest declared tag name, when close enough to be a typo.
fn nearest<'a>(name: &str, candidates: impl Iterator<Item = &'a String>) -> Option<&'a str> {
    let limit = (name.len() / 4).clamp(1, 3);
    candidates
        .map(|c| (edit_distance(name, c), c.as_str()))
        .filter(|(d, _)| *d <= limit)
        .min_by_key(|(d, c)| (*d, c.len()))
        .map(|(_, c)| c)
}

fn edit_distance(a: &str, b: &str) -> usize {
    let (a, b): (Vec<char>, Vec<char>) = (a.chars().collect(), b.chars().collect());
    let mut prev: Vec<usize> = (0..=b.len()).collect();
    let mut cur = vec![0usize; b.len() + 1];
    for (i, ca) in a.iter().enumerate() {
        cur[0] = i + 1;
        for (j, cb) in b.iter().enumerate() {
            let cost = usize::from(ca != cb);
            cur[j + 1] = (prev[j] + cost).min(prev[j + 1] + 1).min(cur[j] + 1);
        }
        std::mem::swap(&mut prev, &mut cur);
    }
    prev[b.len()]
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::lint_document;

    fn schema(src: &str) -> Vec<Diagnostic> {
        lint_document("f.md", src, &Corpus::empty())
            .diagnostics
            .into_iter()
            .filter(|d| {
                [
                    "missing-attribute",
                    "invalid-attribute-type",
                    "invalid-child",
                    "undefined-attribute",
                    "undefined-tag",
                    "schema",
                ]
                .contains(&d.rule.as_ref())
            })
            .collect()
    }

    #[test]
    fn a_declared_tag_is_quiet() {
        // A block: `{% callout %}x{% /callout %}` on one line is inline
        // upstream, which a callout is not.
        assert!(schema("{% callout type=\"note\" %}\nx\n{% /callout %}\n").is_empty());
    }

    #[test]
    fn an_alias_counts_as_declared() {
        // `notice` is an alias of `callout`, `image` of `media`.
        assert!(schema("{% notice type=\"note\" %}\nx\n{% /notice %}\n").is_empty());
        assert!(schema("{% image src=\"a.png\" /%}\n").is_empty());
    }

    #[test]
    fn an_undeclared_tag_is_reported_once() {
        let got = schema("{% colorBox %}x{% /colorBox %}\n");
        assert_eq!(got.len(), 1, "{got:?}");
        assert_eq!(got[0].rule, "undefined-tag");
        assert!(matches!(got[0].severity, Severity::Warning));
        assert!(got[0].message.contains("colorBox"), "{}", got[0].message);
    }

    #[test]
    fn a_near_miss_names_the_tag_it_probably_meant() {
        let got = schema("{% callou type=\"note\" %}\nx\n{% /callou %}\n");
        assert_eq!(got.len(), 1, "{got:?}");
        assert!(got[0].message.contains("'callout'"), "{}", got[0].message);
    }

    #[test]
    fn an_interpolation_is_not_a_tag() {
        // It is a text node whose content is the variable.
        assert!(schema("Text {% $config.model %} more.\n").is_empty());
    }

    #[test]
    fn upstreams_ids_are_reported_under_the_lints_names() {
        let got = schema("{% callout type=\"nope\" kind=1 %}\nx\n{% /callout %}\n");
        let rules: Vec<&str> = got.iter().map(|d| d.rule.as_ref()).collect();
        assert_eq!(rules, vec!["schema", "undefined-attribute"], "{got:?}");
        assert!(got[1].message.contains("'kind'"), "{got:?}");
    }

    #[test]
    fn a_block_tag_written_inline_is_an_error() {
        // `tag-placement-invalid`: upstream's critical error, the lint's
        // `schema` at error level, now that the bundle and the corpus
        // declare where their tags go (MIGRATION-MAP D5, D6).
        let got = schema("Text {% callout %}x{% /callout %} more.\n");
        assert_eq!(got.len(), 1, "{got:?}");
        assert_eq!(got[0].rule, "schema");
        assert_eq!(got[0].severity, Severity::Error);
        assert!(got[0].message.contains("should be block"), "{got:?}");
    }

    #[test]
    fn a_partial_is_left_to_the_partials_rule() {
        // Upstream would say "Partial `x.md` not found" of every partial,
        // because the lint fills no `config.partials` with documents.
        assert!(schema("{% partial file=\"x.md\" /%}\n").is_empty());
        // What else upstream says of one still stands.
        let got = schema("{% partial /%}\n");
        assert_eq!(got.len(), 1, "{got:?}");
        assert_eq!(got[0].rule, "missing-attribute");
    }
}

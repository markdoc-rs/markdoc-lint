//! A configured string written out by hand instead of the variable that
//! supplies it.
//!
//! `{% $company %}` renders "Acme Industrial Robotics". Typing those three
//! words instead works until the company is renamed, the page is
//! white-labelled, or a product's name changes between editions — and then
//! it works in every page that used the variable and nowhere else. This is
//! the rule that finds the ones that did not.
//!
//! ## The parser is most of this rule
//!
//! The version this replaces is 573 lines of TypeScript, and 466 of them
//! rebuild a parser: CommonMark's left- and right-flanking delimiter runs by
//! hand, to find italics; a fence reader; an inline-code reader; span
//! arithmetic to ask whether one range is inside another; a scan for
//! `{% … %}` ranges; an escape counter; a line-number-from-offset helper.
//! Every one of those answers *where am I*, and every one of those questions
//! is a node type. The rule itself — the part that says a literal is a
//! literal — is the eighty lines below.
//!
//! This is [`super::variables`]'s argument a second time: *"the usual
//! regular-expression version carries four patterns … because it has to
//! rebuild enough of a parser."* The parser already knows.
//!
//! ## Why its own `sources`
//!
//! RFC-0002 expected this rule to read `[variables] sources` and need no
//! configuration of its own. It cannot. Those two lists answer different
//! questions. `[variables]` asks *is this name defined anywhere*, so it is
//! deliberately generous: every `*_config.json` in the corpus contributes,
//! because any of them may define a name a page legitimately uses. This
//! rule asks *should this text have been a variable*, which is only true of
//! values that hold corpus-wide. A variant's `manual_config.json` saying
//! `robot = "Bot600"` does not make the words "Bot600" a mistake on a
//! different product's page.
//!
//! So `sources` here names the corpus-wide files, and with none named the
//! rule does nothing.

use crate::rules::{Rule, RuleInfo};
use crate::{Config, Corpus, Diagnostic, Document, Severity, Span, tag_name};
use markdoc::ast::Node;
use markdoc::types::{NodeType, Scalar};
use serde::Deserialize;
use std::collections::BTreeSet;

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct HardcodedConfig {
    /// Files whose top-level string values a page should be writing as a
    /// variable. Empty disables the rule.
    ///
    /// Top-level only, and strings only: a nested object's leaves are
    /// structure rather than prose, and a number written out is not a
    /// literal anybody can mistake for text.
    pub sources: Vec<String>,
    /// Node kinds whose text is left alone, by their Markdoc names.
    ///
    /// The default is the four the TypeScript hard-coded. Emphasis and bold
    /// because a page that *names* a term is usually defining it; code and
    /// fences because a literal in a command is the literal, not a variable
    /// reference that failed to happen.
    pub skip_kinds: Vec<String>,
    /// Tags whose attributes are left alone.
    ///
    /// Media tags, in this corpus: an image's `alt` or a video's `title`
    /// carries the name of a thing, and the renderers do not interpolate
    /// there.
    pub skip_tags: Vec<String>,
}

impl Default for HardcodedConfig {
    fn default() -> Self {
        Self {
            sources: Vec::new(),
            skip_kinds: ["em", "strong", "code", "fence"]
                .iter()
                .map(|s| s.to_string())
                .collect(),
            skip_tags: Vec::new(),
        }
    }
}

pub struct HardcodedVariables;

impl Rule for HardcodedVariables {
    fn describe(&self) -> RuleInfo {
        RuleInfo {
            name: "hardcoded-variables",
            summary: "A configured string written out instead of the variable that supplies it.",
            diagnostics: &["hardcoded-variable"],
            default_severity: Severity::Warning,
            configured_by: Some("[hardcoded-variables] sources"),
        }
    }

    fn configured(&self, config: &Config) -> bool {
        !config.hardcoded.sources.is_empty()
    }

    fn check(&self, doc: &Document<'_>, corpus: &Corpus, out: &mut Vec<Diagnostic>) {
        check(doc, corpus, out);
    }
}

pub fn check(doc: &Document<'_>, index: &Corpus, out: &mut Vec<Diagnostic>) {
    let (rel, tree) = (doc.rel, doc.tree);
    let cfg = &index.config.hardcoded;
    if cfg.sources.is_empty() || index.literals.is_empty() {
        return;
    }
    let skip_kinds: BTreeSet<&str> = cfg.skip_kinds.iter().map(|s| s.as_str()).collect();
    let skip_tags: BTreeSet<&str> = cfg.skip_tags.iter().map(|s| s.as_str()).collect();
    // Frontmatter is not a child of the document node, so walking children
    // skips it without being told to -- which is what the rule wants: those
    // fields hold the literal names, and are where the values come from.
    visit(rel, tree, index, &skip_kinds, &skip_tags, out);
}

fn visit(
    rel: &str,
    node: &Node,
    index: &Corpus,
    skip_kinds: &BTreeSet<&str>,
    skip_tags: &BTreeSet<&str>,
    out: &mut Vec<Diagnostic>,
) {
    if skip_kinds.contains(node.node_type.as_str()) || crate::is_phantom(node) {
        return;
    }
    if let Some(name) = tag_name(node)
        && skip_tags.contains(name)
    {
        return;
    }

    match node.node_type {
        // Running text: the page would have written `{% $key %}`. A text
        // node can also be an interpolation, `{% upper("Bot600") %}`, whose
        // arguments are inside a tag.
        NodeType::Text => match node.attributes.get("content") {
            Some(Scalar::String(text)) => {
                scan(text, index, |at, value, keys| {
                    out.push(report(rel, node, text, at, value, keys, false));
                });
            }
            Some(value) => scan_value(rel, node, value, index, out),
            None => {}
        },
        // Inside a tag the interpolation is bare, so the suggestion is too.
        // Every attribute, into a function's arguments, lists and objects: a
        // condition comparing against a literal is the case this rule was
        // written for.
        NodeType::Tag => {
            for value in node.attributes.values() {
                scan_value(rel, node, value, index, out);
            }
        }
        _ => {}
    }

    for child in &node.children {
        visit(rel, child, index, skip_kinds, skip_tags, out);
    }
    for slot in node.slots.values() {
        visit(rel, slot, index, skip_kinds, skip_tags, out);
    }
}

/// The strings in a value written inside a tag, wherever they are nested.
fn scan_value(rel: &str, node: &Node, value: &Scalar, index: &Corpus, out: &mut Vec<Diagnostic>) {
    match value {
        Scalar::String(text) => scan(text, index, |at, literal, keys| {
            out.push(report(rel, node, text, at, literal, keys, true));
        }),
        Scalar::Function(function) => {
            for argument in function.parameters.values() {
                scan_value(rel, node, argument, index, out);
            }
        }
        Scalar::Array(items) => {
            for item in items {
                scan_value(rel, node, item, index, out);
            }
        }
        Scalar::Object(fields) => {
            for field in fields.values() {
                scan_value(rel, node, field, index, out);
            }
        }
        _ => {}
    }
}

/// Every configured value occurring in `text`, longest first.
///
/// `Corpus::literals` is sorted longest-value-first so that a product called
/// `Bot600` cannot shadow one called `Bot600 Shelf Lift`: the longer name
/// matches at the same position and wins, and the scan then resumes past the
/// whole of it.
fn scan(text: &str, index: &Corpus, mut found: impl FnMut(usize, &str, &[String])) {
    let bytes = text.as_bytes();
    let mut at = 0;
    while at < bytes.len() {
        let mut step = 1;
        for (value, keys) in &index.literals {
            if value.as_bytes()[0] != bytes[at] {
                continue;
            }
            let end = at + value.len();
            if end > bytes.len() || &bytes[at..end] != value.as_bytes() || !bounded(text, at, end) {
                continue;
            }
            found(at, value, keys);
            step = value.len();
            break;
        }
        at += step;
    }
}

/// A match has to be the whole word. "Bot100" inside "Bot1000" is not a
/// hardcoded "Bot100", and reporting it would teach a writer to stop reading
/// this rule's output.
fn bounded(text: &str, start: usize, end: usize) -> bool {
    let word = |c: char| c.is_ascii_alphanumeric() || c == '_';
    let before = text[..start].chars().next_back();
    let after = text[end..].chars().next();
    !before.is_some_and(word) && !after.is_some_and(word)
}

fn report(
    rel: &str,
    node: &Node,
    text: &str,
    at: usize,
    value: &str,
    keys: &[String],
    inside_tag: bool,
) -> Diagnostic {
    let suggestion = keys
        .iter()
        .map(|key| {
            if inside_tag {
                format!("${key}")
            } else {
                format!("{{% ${key} %}}")
            }
        })
        .collect::<Vec<_>>()
        .join(" or ");
    Diagnostic::new(
        rel,
        Severity::Warning,
        "hardcoded-variable",
        format!("`{value}` is written out here; it is what {suggestion} renders"),
    )
    .at(position(node, text, at, value))
    .help(format!(
        "Writing it out means this page keeps the old wording when the value changes. Use {suggestion}."
    ))
}

/// Where in the file the match is.
///
/// The node carries its own start; the offset inside its text is added by
/// counting the newlines before it. Exact whenever the node's text is the
/// source text, which is the ordinary case, and off by the width of an
/// escape or an entity when it is not -- or, inside a tag, by where in the
/// tag the value was written. A position inside the right node is worth
/// having even then -- the alternative is pointing at the start of a
/// paragraph and letting the reader find it.
fn position(node: &Node, text: &str, at: usize, value: &str) -> Option<Span> {
    let node = Span::of_node(node)?;
    let before = &text[..at];
    let lines = before.matches('\n').count();
    let start_line = node.start_line + lines;
    let start_char = match before.rfind('\n') {
        Some(nl) => before[nl + 1..].encode_utf16().count(),
        None => node.start_char + before.encode_utf16().count(),
    };
    let width = value.encode_utf16().count();
    Some(Span {
        start_line,
        start_char,
        end_line: start_line,
        end_char: start_char + width,
        start_offset: node.start_offset + at,
        end_offset: node.start_offset + at + value.len(),
    })
}

#[cfg(test)]
mod tests {
    use crate::{Config, Corpus, lint_document};

    fn corpus(extra: &str) -> Corpus {
        let config = Config::parse(&format!(
            "[hardcoded-variables]\nsources = [\"global.config.json\"]\n{extra}"
        ))
        .unwrap();
        Corpus {
            config,
            literals: vec![
                (
                    "Acme Industrial Robotics".to_string(),
                    vec!["company".to_string()],
                ),
                ("Bot600 Shelf Lift".to_string(), vec!["shelf".to_string()]),
                ("Bot600".to_string(), vec!["robot".to_string()]),
            ],
            ..Default::default()
        }
    }

    fn findings(corpus: &Corpus, src: &str) -> Vec<String> {
        lint_document("a.md", src, corpus)
            .diagnostics
            .into_iter()
            .filter(|d| d.rule == "hardcoded-variable")
            .map(|d| d.message)
            .collect()
    }

    #[test]
    fn running_text_is_told_to_interpolate() {
        let got = findings(&corpus(""), "Sold by Acme Industrial Robotics today.\n");
        assert_eq!(got.len(), 1, "{got:?}");
        assert!(got[0].contains("{% $company %}"), "{got:?}");
    }

    #[test]
    fn inside_a_tag_the_suggestion_is_bare() {
        // A condition testing the literal: `{% $robot %}` there would be a
        // syntax error, so the advice has to differ.
        let got = findings(
            &corpus(""),
            "{% if equals($x, \"Bot600\") %}\nyes\n{% /if %}\n",
        );
        assert_eq!(got.len(), 1, "{got:?}");
        assert!(got[0].contains("$robot"), "{got:?}");
        assert!(!got[0].contains("{%"), "bare inside a tag: {got:?}");
    }

    #[test]
    fn the_longest_value_wins() {
        // `Bot600` is a prefix of `Bot600 Shelf Lift`. Reporting the short
        // one would send the writer to the wrong variable.
        let got = findings(&corpus(""), "The Bot600 Shelf Lift is here.\n");
        assert_eq!(got.len(), 1, "{got:?}");
        assert!(got[0].contains("$shelf"), "{got:?}");
    }

    #[test]
    fn a_longer_word_is_not_a_match() {
        assert!(findings(&corpus(""), "The Bot6000 is not a robot.\n").is_empty());
    }

    #[test]
    fn emphasis_and_code_are_left_alone() {
        // A page defining the term, and a command that contains it.
        assert!(findings(&corpus(""), "*Acme Industrial Robotics* is the company.\n").is_empty());
        assert!(findings(&corpus(""), "Run `ssh Bot600` to connect.\n").is_empty());
        assert!(findings(&corpus(""), "```\nping Bot600\n```\n").is_empty());
    }

    #[test]
    fn a_skipped_tag_keeps_its_attributes() {
        let c = corpus("skip_tags = [\"image\"]\n");
        assert!(findings(&c, "{% image alt=\"Bot600\" /%}\n").is_empty());
        // And a tag that is not skipped still reports.
        assert_eq!(
            findings(&c, "{% callout title=\"Bot600\" %}\nx\n{% /callout %}\n").len(),
            1
        );
    }

    #[test]
    fn frontmatter_is_where_the_values_live_and_is_not_reported() {
        let got = findings(
            &corpus(""),
            "---\ntitle: Bot600\nrobot: Acme Industrial Robotics\n---\n\nBody.\n",
        );
        assert!(got.is_empty(), "{got:?}");
    }

    #[test]
    fn with_no_sources_the_rule_says_nothing() {
        let c = Corpus {
            literals: vec![("Bot600".to_string(), vec!["robot".to_string()])],
            ..Default::default()
        };
        assert!(findings(&c, "The Bot600 is here.\n").is_empty());
    }

    #[test]
    fn the_position_points_at_the_literal_not_the_paragraph() {
        let linted = lint_document(
            "a.md",
            "First line.\nSecond line mentions Bot600 here.\n",
            &corpus(""),
        );
        let d = linted
            .diagnostics
            .iter()
            .find(|d| d.rule == "hardcoded-variable")
            .expect("a finding");
        let span = d.span.expect("a span");
        assert_eq!(span.start_line, 2, "{d:?}");
        assert_eq!(span.start_char, 21, "{d:?}");
    }
}

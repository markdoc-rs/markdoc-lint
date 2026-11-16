//! Lint rules for a Markdoc corpus, over the real AST.
//!
//! ## Why an AST and not regular expressions
//!
//! Most checks of this kind are written as regular expressions over the file
//! text, and they all end up re-implementing a parser badly in order to
//! avoid one. A rule that wants to know whether a `{% tagref %}` resolves
//! has to tell a fenced code block from prose; a rule about variables inside
//! `{% if %}` has to track nesting to know which `{% /if %}` closes which
//! `{% if %}`. The parser already knows both.
//!
//! So a rule here asks the tree a question. That also means a diagnostic can
//! say *where* — every node carries its source position — and an editor can
//! jump to it.
//!
//! ## Which parser
//!
//! `markdoc`, the port of upstream Markdoc 0.5.11, with the corpus's
//! own options (`markdoc::ext`, see [`Corpus`]): the same tree, tags and
//! pairing the renderers and the editor get, because every tool reading the
//! corpus parses it with the same `[parse]` table. A tag inside a fence is
//! live upstream (unless the fence says `process=false`); the convention
//! rules still treat a fence's content as an example, see [`walk`].
//!
//! ## What a rule is
//!
//! A [`rules::Rule`]: it describes itself — its name, every `Diagnostic.rule`
//! it can emit, and the configuration that turns it on — and checks one
//! document against a [`Corpus`] index, pushing [`Diagnostic`]s. The
//! description is the part that earns a trait, and [`rules`] says why a list
//! of function calls was not enough.
//!
//! ## One entry point, two callers
//!
//! [`lint_document`] takes a file and returns diagnostics. A CLI calls it
//! over every file; a language server calls it over the one the writer has
//! open, on every keystroke. That they are the same function is the point —
//! an editor that disagrees with CI about whether a document is clean is
//! worse than no editor integration at all.
//!
//! ## A report beside the lint
//!
//! [`Vocabulary`] reads the same documents with the same options and says
//! what the corpus writes rather than what is wrong with it. See
//! [`vocabulary`].

pub mod config;
pub mod corpus;
pub mod rules;
pub mod vocabulary;

pub use config::Config;
pub use corpus::Corpus;
pub use rules::{CorpusRule, Rule, RuleInfo, RuleSettings, RuleStatus};
pub use vocabulary::Vocabulary;

use markdoc::IndexMap;
use markdoc::ast::Node;
use markdoc::ext::Occurrence;
use markdoc::types::{NodeType, Span as SourceSpan, ValidationError};
use serde::{Deserialize, Serialize};
use std::borrow::Cow;

/// How much a finding matters.
///
/// Maps onto both the LSP's `DiagnosticSeverity` and a process exit code,
/// which is why there are exactly these three and not a richer set: a writer
/// needs to know whether the build will refuse their page, whether it will
/// complain, or whether this is advice.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Severity {
    /// The document is wrong and something downstream will be wrong too.
    Error,
    /// Suspicious, and worth a human deciding.
    Warning,
    /// House style.
    Hint,
}

impl Severity {
    pub fn label(self) -> &'static str {
        match self {
            Severity::Error => "error",
            Severity::Warning => "warning",
            Severity::Hint => "hint",
        }
    }
}

/// Where a finding is, in the file the author has open.
///
/// Both coordinate systems are carried because both callers want a different
/// one and converting is not free: an editor counts lines and UTF-16
/// columns, a terminal prints line and column, and a fixer slices bytes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Span {
    /// 1-based.
    pub start_line: usize,
    /// 0-based, UTF-16 code units.
    pub start_char: usize,
    pub end_line: usize,
    pub end_char: usize,
    pub start_offset: usize,
    pub end_offset: usize,
}

impl Span {
    /// markdoc's own position of a node, an error or a tag
    /// (`markdoc::types::Span`), in this crate's terms.
    pub fn from_span(span: &SourceSpan) -> Self {
        Self {
            start_line: span.start.line,
            start_char: span.start.character.unwrap_or(0),
            end_line: span.end.line,
            end_char: span.end.character.unwrap_or(0),
            start_offset: span.start.offset,
            end_offset: span.end.offset,
        }
    }

    /// The start of one line, for a finding that has no narrower place to
    /// point at — a missing frontmatter field, say, which is about the
    /// absence of text rather than about any text.
    pub fn line(line: usize) -> Self {
        Self {
            start_line: line,
            start_char: 0,
            end_line: line,
            end_char: 0,
            start_offset: 0,
            end_offset: 0,
        }
    }

    // Every position a rule reads comes through the three functions below,
    // and every one is markdoc's own (1-based lines, UTF-16 columns, byte
    // offsets), which it keeps in `span` and computes only when the parse
    // asks for it (`ParseOptions::with_spans`): `lint_document` and the
    // corpus's anchor index ask; a tree parsed without has none. A node's
    // and an error's `location` are upstream's, which has no offsets and no
    // columns a rule could use.

    /// Where a node is.
    pub fn of_node(node: &Node) -> Option<Self> {
        node.span.as_ref().map(Self::from_span)
    }

    /// Where the parser or the validator placed an error, if it did.
    pub fn of_error(error: &ValidationError) -> Option<Self> {
        error.span.as_ref().map(Self::from_span)
    }

    /// Where a `{% %}` is, from `{%` to `%}`.
    pub fn of_occurrence(occurrence: &Occurrence) -> Self {
        Self::from_span(&occurrence.location)
    }
}

/// One finding.
///
/// `Deserialize` as well as `Serialize`, because this type now travels in
/// both directions: `markdoc-lint` and `markdoc-prose` write it, and
/// `tw check` reads it back from whatever `[[check.external]]` names. A tool
/// that owns a format exposes `--check --json` and emits these, so the
/// format has one definition rather than one per tool — which is the
/// argument this workspace is built on, turned on its own output.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Diagnostic {
    /// Corpus-relative, forward slashes, so a diagnostic reads the same on
    /// every machine and in CI.
    pub file: String,
    pub severity: Severity,
    /// A stable kebab-case identifier. It appears in output, in an editor,
    /// and in whatever somebody greps for when a rule fires too often, so it
    /// may not be renamed casually.
    ///
    /// `Cow` rather than `&'static str`: a built-in rule's name is a literal
    /// and still costs nothing, while a name that arrived from somewhere
    /// else — another tool's JSON, or a rule list read at runtime — can be
    /// owned instead of leaked. It serialises as a plain string either way,
    /// so the wire format is unchanged.
    pub rule: Cow<'static, str>,
    pub message: String,
    pub span: Option<Span>,
    /// What to do about it. Separate from `message` because an editor shows
    /// the message in a tooltip and the help only when asked, and because a
    /// message that ends in advice is harder to read when it fires 200
    /// times.
    pub help: Option<String>,
}

impl Diagnostic {
    pub fn new(
        file: impl Into<String>,
        severity: Severity,
        rule: impl Into<Cow<'static, str>>,
        message: impl Into<String>,
    ) -> Self {
        Self {
            file: file.into(),
            severity,
            rule: rule.into(),
            message: message.into(),
            span: None,
            help: None,
        }
    }

    pub fn at(mut self, span: Option<Span>) -> Self {
        self.span = span;
        self
    }

    pub fn at_span(self, span: Option<&SourceSpan>) -> Self {
        let span = span.map(Span::from_span);
        self.at(span)
    }

    pub fn help(mut self, help: impl Into<String>) -> Self {
        self.help = Some(help.into());
        self
    }

    /// `path:line:col: severity[rule] message`, the shape every compiler and
    /// editor already knows how to jump to.
    pub fn to_text(&self) -> String {
        let where_ = match &self.span {
            Some(s) => format!("{}:{}:{}", self.file, s.start_line, s.start_char + 1),
            None => self.file.clone(),
        };
        let mut out = format!(
            "{where_}: {}[{}] {}",
            self.severity.label(),
            self.rule,
            self.message
        );
        if let Some(h) = &self.help {
            out.push_str(&format!("\n    help: {h}"));
        }
        out
    }
}

/// Everything found in one document.
pub struct Linted {
    pub tree: Node,
    /// Every `{% %}` the tokenizer recognised, with the parser's pairing --
    /// what an editor colours and pairs, from the same tokenizing as `tree`.
    pub occurrences: Vec<Occurrence>,
    pub diagnostics: Vec<Diagnostic>,
}

/// One document, as every rule sees it.
pub struct Document<'a> {
    /// Corpus-relative, forward slashes.
    pub rel: &'a str,
    /// The text as read.
    pub source: &'a str,
    /// Its tree, parsed with the corpus's options ([`Corpus::parse_options`]).
    pub tree: &'a Node,
    /// Every `{% %}` the tokenizer recognised, in source order, with the
    /// parser's pairing (`markdoc::ext::occurrences`). The tree cannot say
    /// where a closing tag or an annotation was, or which tag a fault
    /// inside a paragraph left open; these can.
    pub occurrences: &'a [Occurrence],
}

/// Lint one document.
///
/// `rel` is the corpus-relative path, which several rules read: whether a
/// file is a partial, which subtree's anchors it may reference, and which
/// frontmatter fields its type requires are decided by where it sits as much
/// as by what is in it.
///
/// The parse is returned alongside the diagnostics because every caller
/// wants it: a CLI to decide an exit code, a language server to answer the
/// hover that follows.
pub fn lint_document(rel: &str, source: &str, corpus: &Corpus) -> Linted {
    let mut diagnostics = Vec::new();
    let config = &corpus.config;

    // With markdoc's own positions (`span`), which it computes only when
    // asked: every finding is placed by them, and so is every answer the
    // language server gives from the tree returned here.
    let options = corpus.parse_options.with_file(rel).with_spans(true);
    let (tree, occurrences) = match markdoc::ext::parse_with_occurrences(source, &options) {
        Ok(parsed) => parsed,
        Err(e) => {
            // The parser refuses a document only where upstream Markdoc
            // throws: nesting past a depth limit (a RangeError), or a tag
            // that is given both `.class` and `class=` (a TypeError).
            // Everything else -- frontmatter that is not YAML included -- it
            // reports on the tree as it goes, and the rules say it.
            diagnostics.push(
                Diagnostic::new(
                    rel,
                    Severity::Error,
                    "unparsable",
                    markdoc::thrown(&e).to_string(),
                )
                .at(Some(Span::line(1))),
            );
            return Linted {
                tree: Node::new(NodeType::Document, IndexMap::new(), Vec::new(), None),
                occurrences: Vec::new(),
                diagnostics,
            };
        }
    };
    let document = Document {
        rel,
        source,
        tree: &tree,
        occurrences: &occurrences,
    };

    // Every rule, in registry order. The two language-level rules always
    // run — they are about Markdoc, not about any project's conventions —
    // and the four convention rules each return immediately when
    // unconfigured, so the loop does not have to know which is which.
    for rule in rules::ALL {
        let info = rule.describe();
        let settings = config.rule_settings(info.name);
        if settings.is_some_and(|s| !s.is_enabled()) {
            continue;
        }
        let first = diagnostics.len();
        rule.check(&document, corpus, &mut diagnostics);
        // An override replaces the severity of everything this rule just
        // produced. Applied here rather than inside the rule so that a rule
        // never has to care, and so a rule from somewhere else gets the
        // same treatment without having implemented it.
        if let Some(severity) = settings.and_then(|s| s.severity) {
            for d in &mut diagnostics[first..] {
                d.severity = severity;
            }
        }
    }

    // Deterministic order: by position, then by rule. Two runs over the same
    // file must print the same thing, or a diff of CI output is noise.
    diagnostics.sort_by(|a, b| {
        let ka = a
            .span
            .map(|s| (s.start_line, s.start_char))
            .unwrap_or((0, 0));
        let kb = b
            .span
            .map(|s| (s.start_line, s.start_char))
            .unwrap_or((0, 0));
        ka.cmp(&kb).then_with(|| a.rule.cmp(&b.rule))
    });

    Linted {
        tree,
        occurrences,
        diagnostics,
    }
}

/// Check the corpus itself: the rules that are about its shape rather than
/// about any one document.
///
/// Called once, by a caller checking a whole corpus. A caller checking named
/// files does not call it — a finding about an empty directory is not about
/// the file somebody staged, and a hook that reported it would be reporting
/// somebody else's work.
pub fn lint_corpus(corpus: &Corpus) -> Vec<Diagnostic> {
    let mut diagnostics = Vec::new();
    for rule in rules::CORPUS_RULES {
        let info = rule.describe();
        let settings = corpus.config.rule_settings(info.name);
        if settings.is_some_and(|s| !s.is_enabled()) {
            continue;
        }
        let first = diagnostics.len();
        rule.check(corpus, &mut diagnostics);
        if let Some(severity) = settings.and_then(|s| s.severity) {
            for d in &mut diagnostics[first..] {
                d.severity = severity;
            }
        }
    }
    diagnostics.sort_by(|a, b| a.file.cmp(&b.file).then_with(|| a.rule.cmp(&b.rule)));
    diagnostics
}

/// Walk a tree as the convention rules read it: every node, the root
/// included, except two kinds that are not an author's nodes.
///
/// - **A fence's content.** Upstream Markdoc processes the tags in a fence
///   unless it says `process=false`, so a `{% tag "x" /%}` there is live to
///   the renderers. This crate's premise is that a fenced block is an
///   example -- the page that documents a convention is full of them -- so
///   the anchors, partials and variables rules, and the corpus index, do not
///   look inside one. The `syntax` and `schema` rules read the whole tree.
/// - **A phantom node.** A closing tag with nothing open to close becomes a
///   node of its own, carrying `missing-opening` and no attributes; read as
///   a tag it would be an anchor without an id or a partial without a file.
///   `syntax` reports it as what it is.
pub fn walk(node: &Node, f: &mut impl FnMut(&Node)) {
    if is_phantom(node) {
        return;
    }
    f(node);
    if node.node_type == NodeType::Fence {
        return;
    }
    for child in &node.children {
        walk(child, f);
    }
    for slot in node.slots.values() {
        walk(slot, f);
    }
}

/// Whether the parser made this node of a closing tag that closed nothing.
pub fn is_phantom(node: &Node) -> bool {
    node.errors.iter().any(|e| e.id == "missing-opening")
}

/// A tag node's name, if it is a tag.
pub fn tag_name(node: &Node) -> Option<&str> {
    match node.node_type {
        NodeType::Tag => node.tag.as_deref(),
        _ => None,
    }
}

/// A string attribute, if present and not blank.
pub fn str_attr<'a>(node: &'a Node, key: &str) -> Option<&'a str> {
    match node.attributes.get(key) {
        Some(markdoc::types::Scalar::String(s)) if !s.trim().is_empty() => Some(s.as_str()),
        _ => None,
    }
}

/// The first of `keys` this node carries as a non-blank string.
pub fn first_str_attr<'a>(node: &'a Node, keys: &[String]) -> Option<&'a str> {
    keys.iter().find_map(|k| str_attr(node, k))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_diagnostic_survives_the_round_trip_another_tool_puts_it_through() {
        // The `[[check.external]]` contract: a tool that owns a format emits
        // these under `--check --json`, and `tw check` reads them back. If
        // the two directions ever disagree the external check reports
        // nothing and says so in no way at all, so the agreement is a test
        // rather than a comment.
        let before = Diagnostic::new(
            "products/a/manual/b.md",
            Severity::Warning,
            "nav-unlisted-page",
            "no sidebar names this page",
        )
        .at(Some(Span::line(1)))
        .help("add it to a manual_nav.json, or delete it");

        let wire = serde_json::to_string(&before).unwrap();
        let after: Diagnostic = serde_json::from_str(&wire).unwrap();

        assert_eq!(after.file, before.file);
        assert_eq!(after.severity, before.severity);
        assert_eq!(after.rule, before.rule);
        assert_eq!(after.message, before.message);
        assert_eq!(after.span, before.span);
        assert_eq!(after.help, before.help);
        // And the name arrived owned rather than leaked, which is the whole
        // reason the field is a `Cow`.
        assert!(matches!(after.rule, std::borrow::Cow::Owned(_)));
        // A built-in rule still costs no allocation.
        assert!(matches!(before.rule, std::borrow::Cow::Borrowed(_)));
    }

    #[test]
    fn the_wire_format_did_not_change_when_the_field_did() {
        // `Cow` serialises as a bare string. Anything already parsing
        // markdoc-lint --json keeps working.
        let d = Diagnostic::new("a.md", Severity::Error, "unclosed-tag", "x");
        let json: serde_json::Value = serde_json::to_value(&d).unwrap();
        assert_eq!(json["rule"], serde_json::json!("unclosed-tag"));
    }

    #[test]
    fn a_diagnostic_prints_where_an_editor_can_jump_to_it() {
        let d = Diagnostic::new("a/b.md", Severity::Error, "demo", "something")
            .at(Some(Span {
                start_line: 12,
                start_char: 4,
                end_line: 12,
                end_char: 9,
                start_offset: 100,
                end_offset: 105,
            }))
            .help("do the other thing");
        assert_eq!(
            d.to_text(),
            "a/b.md:12:5: error[demo] something\n    help: do the other thing"
        );
    }

    #[test]
    fn a_diagnostic_without_a_position_still_names_the_file() {
        let d = Diagnostic::new("a/b.md", Severity::Warning, "demo", "something");
        assert_eq!(d.to_text(), "a/b.md: warning[demo] something");
    }

    #[test]
    fn severities_order_worst_first() {
        let mut v = vec![Severity::Hint, Severity::Error, Severity::Warning];
        v.sort();
        assert_eq!(v, vec![Severity::Error, Severity::Warning, Severity::Hint]);
    }

    fn corpus_with(toml: &str) -> Corpus {
        Corpus {
            config: Config::parse(toml).unwrap(),
            ..Default::default()
        }
    }

    #[test]
    fn a_corpus_can_switch_a_rule_off() {
        // Not hypothetical: a corpus adopting this linter against an
        // existing body of work has a backlog, and the choice is between
        // switching one rule off deliberately and everybody learning to
        // pass --no-verify.
        let on = lint_document("a.md", "{% callout %}\nbody\n", &Corpus::empty());
        assert_eq!(on.diagnostics.len(), 1);

        let off = corpus_with("[rules.syntax]\nenabled = false\n");
        let linted = lint_document("a.md", "{% callout %}\nbody\n", &off);
        assert!(linted.diagnostics.is_empty(), "{:?}", linted.diagnostics);
    }

    #[test]
    fn an_override_re_levels_every_finding_from_that_rule_and_no_other() {
        // "Report it, but do not stop me", said where every other
        // severity lives. The document has an unclosed tag (syntax, error)
        // and an undeclared tag (schema, warning); re-levelling one must
        // not move the other.
        let corpus = corpus_with("[rules.syntax]\nseverity = \"hint\"\n");
        let linted = lint_document("a.md", "{% nosuchtag %}\nbody\n", &corpus);
        let got: Vec<(&str, Severity)> = linted
            .diagnostics
            .iter()
            .map(|d| (d.rule.as_ref(), d.severity))
            .collect();
        assert!(
            got.contains(&("unclosed-tag", Severity::Hint)),
            "syntax not re-levelled: {got:?}"
        );
        assert!(
            got.contains(&("undefined-tag", Severity::Warning)),
            "schema moved with it: {got:?}"
        );
    }

    #[test]
    fn an_unconfigured_corpus_reports_only_language_faults() {
        // The upstream default. A tag that never closes is Markdoc's rule
        // and fires; an unresolved `{% tagref %}` is a convention and does
        // not, because nothing has said what the convention is.
        let c = Corpus::empty();
        let linted = lint_document("a.md", "{% callout %}\nbody\n", &c);
        assert_eq!(
            linted
                .diagnostics
                .iter()
                .map(|d| d.rule.as_ref())
                .collect::<Vec<_>>(),
            vec!["unclosed-tag"]
        );
        let linted = lint_document("a.md", "{% tagref \"nope\" /%}\n", &c);
        assert!(linted.diagnostics.is_empty(), "{:?}", linted.diagnostics);
    }
}

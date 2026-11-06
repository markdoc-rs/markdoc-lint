//! `{% $variable %}` — is it a name anything supplies.
//!
//! This is the rule where the AST earns its keep most plainly. The usual
//! regular-expression version of this check carries four patterns —
//! open-if, else, close-if, variable — and a nesting counter, because it has
//! to rebuild enough of a parser to know which `{% /if %}` closes which
//! `{% if %}`. The parser already knows: a conditional's branches are its
//! children, and a variable is a value of its own, wherever it is written --
//! an attribute, a function's argument, a list, or the text node that
//! `{% $name %}` becomes.

use crate::rules::{Rule, RuleInfo};
use crate::{Config, Corpus, Diagnostic, Document, Severity, Span, walk};
use markdoc::ast::PathSegment;
use markdoc::types::Scalar;
use serde::Deserialize;

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct VariablesConfig {
    /// Filenames supplying variable names. `*` is allowed, as in
    /// `*_config.json`. Empty disables the variable rule.
    pub sources: Vec<String>,
    /// Whether a document's own frontmatter keys count as defined
    /// variables for the pages that inherit them.
    pub from_frontmatter: bool,
    /// Names that are always defined, whatever the sources say — for
    /// variables a renderer injects rather than a file declaring them.
    pub extra: Vec<String>,
}

impl Default for VariablesConfig {
    fn default() -> Self {
        Self {
            sources: Vec::new(),
            from_frontmatter: true,
            extra: Vec::new(),
        }
    }
}

pub struct Variables;

impl Rule for Variables {
    fn describe(&self) -> RuleInfo {
        RuleInfo {
            name: "variables",
            summary: "Every `{% $name %}` is a name some source supplies.",
            diagnostics: &["unknown-variable"],
            default_severity: Severity::Error,
            configured_by: Some("[variables] sources"),
        }
    }

    fn configured(&self, config: &Config) -> bool {
        !config.variables.sources.is_empty()
    }

    fn check(&self, doc: &Document<'_>, corpus: &Corpus, out: &mut Vec<Diagnostic>) {
        check(doc, corpus, out);
    }
}

pub fn check(doc: &Document<'_>, index: &Corpus, out: &mut Vec<Diagnostic>) {
    // No sources configured means no list of defined names, and a rule that
    // reports every variable in the file as unknown is a rule a writer turns
    // off.
    if index.config.variables.sources.is_empty() || !index.is_indexed() {
        return;
    }
    let rel = doc.rel;

    walk(doc.tree, &mut |node| {
        for (attr, value) in &node.attributes {
            let mut names = Vec::new();
            variable_names(value, &mut names);
            for name in names {
                if is_known(&name, index) {
                    continue;
                }
                // `primary` is the value after a tag's name, and `content`
                // is a text node's: what was written is the variable itself.
                let what = if attr == "primary" || attr == "content" {
                    String::new()
                } else {
                    format!(" (in `{attr}`)")
                };
                out.push(
                    Diagnostic::new(
                        rel,
                        Severity::Error,
                        "unknown-variable",
                        format!("`${name}` is not a variable anything defines{what}."),
                    )
                    .at(Span::of_node(node))
                    .help("A typo here renders as nothing at all rather than as an error."),
                );
            }
        }
    });
}

/// A name counts as known if it, or any prefix of it, is defined.
///
/// `$robot.name` is fine when `robot` is an object: the config supplies the
/// object and the page reaches into it. Checking only the full dotted path
/// would report every field access in the corpus.
fn is_known(name: &str, index: &Corpus) -> bool {
    if index.variables.contains(name) {
        return true;
    }
    let mut prefix = String::new();
    for part in name.split('.') {
        if !prefix.is_empty() {
            prefix.push('.');
        }
        prefix.push_str(part);
        if index.variables.contains(&prefix) {
            return true;
        }
    }
    false
}

/// Every variable a value mentions, as a dotted name: `$a.b[0]` is `a.b.0`.
///
/// Into a function's arguments, lists and objects, because that is where a
/// condition keeps them: `{% if equals($product, "x") %}`.
fn variable_names(value: &Scalar, out: &mut Vec<String>) {
    match value {
        Scalar::Variable(variable) => {
            let name = variable
                .path
                .iter()
                .map(|segment| match segment {
                    PathSegment::String(s) => s.clone(),
                    PathSegment::Number(n) => format!("{n}"),
                })
                .collect::<Vec<_>>()
                .join(".");
            if !name.is_empty() && !out.contains(&name) {
                out.push(name);
            }
        }
        Scalar::Function(function) => {
            for argument in function.parameters.values() {
                variable_names(argument, out);
            }
        }
        Scalar::Array(items) => {
            for item in items {
                variable_names(item, out);
            }
        }
        Scalar::Object(fields) => {
            for field in fields.values() {
                variable_names(field, out);
            }
        }
        _ => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Config;
    use std::collections::BTreeSet;

    fn index_with(names: &[&str]) -> Corpus {
        Corpus {
            config: Config::parse("[variables]\nsources = [\"global.config.json\"]\n").unwrap(),
            variables: names.iter().map(|s| s.to_string()).collect::<BTreeSet<_>>(),
            files: 1,
            ..Default::default()
        }
    }

    fn unknown(idx: &Corpus, src: &str) -> Vec<String> {
        crate::lint_document("x.md", src, idx)
            .diagnostics
            .into_iter()
            .filter(|d| d.rule == "unknown-variable")
            .map(|d| d.message)
            .collect()
    }

    #[test]
    fn names_are_read_out_of_every_value_that_can_hold_one() {
        let idx = index_with(&[]);
        let got = unknown(
            &idx,
            "{% if equals($a.b, [$c, {k: $d}]) %}\nx\n{% /if %}\n\nThe {% $e[0] %}.\n",
        );
        assert_eq!(got.len(), 4, "{got:?}");
        for name in ["`$a.b`", "`$c`", "`$d`", "`$e.0`"] {
            assert!(got.iter().any(|m| m.contains(name)), "{name}: {got:?}");
        }
    }

    #[test]
    fn a_half_typed_path_is_the_syntax_rules() {
        // What an editor sees mid-keystroke. It is no variable at all now
        // but a tag the grammar cannot read, which `syntax` reports.
        let idx = index_with(&["robot"]);
        let linted = crate::lint_document("x.md", "The {% $robot. %} here.\n", &idx);
        let rules: Vec<&str> = linted.diagnostics.iter().map(|d| d.rule.as_ref()).collect();
        assert_eq!(rules, vec!["syntax"], "{:?}", linted.diagnostics);
    }

    #[test]
    fn reaching_into_a_defined_object_is_known() {
        let idx = index_with(&["robot", "company"]);
        assert!(is_known("robot", &idx));
        assert!(is_known("robot.name", &idx));
        assert!(is_known("robot.name.first", &idx));
        assert!(!is_known("rob", &idx));
        assert!(!is_known("nothing", &idx));
    }

    #[test]
    fn an_unknown_variable_is_reported_with_its_position() {
        let idx = index_with(&["a"]);
        let linted = crate::lint_document("x.md", "Text.\n\nThe {% $b %} is here.\n", &idx);
        let found: Vec<_> = linted
            .diagnostics
            .iter()
            .filter(|d| d.rule == "unknown-variable")
            .collect();
        assert_eq!(found.len(), 1, "{:?}", linted.diagnostics);
        assert!(found[0].message.contains("$b"));
        assert!(!found[0].message.contains("(in "), "{}", found[0].message);
        // At the interpolation itself.
        let span = found[0].span.unwrap();
        assert_eq!((span.start_line, span.start_char), (3, 4));
    }

    #[test]
    fn a_variable_in_an_attribute_says_which() {
        let idx = index_with(&["a"]);
        let got = unknown(&idx, "{% callout type=$kind %}\nx\n{% /callout %}\n");
        assert_eq!(got.len(), 1, "{got:?}");
        assert!(got[0].contains("(in `type`)"), "{got:?}");
    }

    #[test]
    fn nothing_is_said_when_no_sources_are_configured() {
        let linted = crate::lint_document("a.md", "{% $anything %}\n", &Corpus::empty());
        assert!(
            !linted
                .diagnostics
                .iter()
                .any(|d| d.rule == "unknown-variable")
        );
    }
}

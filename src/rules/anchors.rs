//! Anchors and the references that point at them.
//!
//! A corpus that cross-references itself needs a way for one page to name a
//! heading in another. Markdoc has no such tag, so projects define a pair:
//! one tag declares an id, another resolves it. `markdoc-lint.toml` names
//! the two, and with neither named this rule does nothing.
//!
//! What the AST buys here is not elegance. A regular expression cannot tell
//! a declaration in a fenced code block from one in prose, and the page that
//! documents the convention is full of the former. (Upstream Markdoc does
//! process the tags in a fence, unless it says `process=false`; this rule
//! treats them as the examples they are, see [`crate::walk`].)

use crate::rules::{Rule, RuleInfo};
use crate::{Config, Corpus, Diagnostic, Document, Severity, Span, first_str_attr, tag_name, walk};
use serde::Deserialize;
use std::collections::HashMap;

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct AnchorsConfig {
    /// The tag that declares an anchor. `None` disables the anchor rules.
    pub declare_tag: Option<String>,
    /// The tag that references one.
    pub reference_tag: Option<String>,
    /// Attributes that may carry the id, in the order they are tried.
    pub id_attributes: Vec<String>,
    pub scope: Scope,
}

impl Default for AnchorsConfig {
    fn default() -> Self {
        Self {
            declare_tag: None,
            reference_tag: None,
            id_attributes: vec!["primary".to_string(), "id".to_string()],
            scope: Scope::Global,
        }
    }
}

impl AnchorsConfig {
    /// Which namespace a file's anchors and references belong to.
    pub fn scope_of(&self, rel: &str) -> String {
        match &self.scope {
            Scope::Global => GLOBAL_SCOPE.to_string(),
            Scope::PathPrefix { under, depth } => {
                let parts: Vec<&str> = rel.split('/').collect();
                let want = under.as_ref().map(|u| u.split('/').count()).unwrap_or(0);
                if let Some(u) = under
                    && !rel.starts_with(&format!("{u}/"))
                {
                    return GLOBAL_SCOPE.to_string();
                }
                let take = want + depth;
                if parts.len() > take && parts[..take].iter().all(|p| !p.is_empty()) {
                    parts[..take].join("/")
                } else {
                    GLOBAL_SCOPE.to_string()
                }
            }
        }
    }
}

/// How far an anchor reaches.
///
/// `Global` is one namespace for the whole corpus. `PathPrefix` gives each
/// subtree its own, which is what a corpus documenting several products
/// wants: two products may both declare `safety_contactors` and mean
/// different pages, and a reference that silently resolved to the other
/// product's page would be worse than one that failed.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(tag = "kind", rename_all = "kebab-case")]
pub enum Scope {
    #[default]
    Global,
    PathPrefix {
        /// Only paths under this prefix are scoped; everything else shares
        /// the global namespace. `None` scopes from the corpus root.
        #[serde(default)]
        under: Option<String>,
        /// How many path segments *after* `under` make the scope.
        depth: usize,
    },
}

/// The scope name for everything that is not under a scoping prefix. Not a
/// path, so it cannot collide with one.
pub const GLOBAL_SCOPE: &str = "<global>";

pub struct Anchors;

impl Rule for Anchors {
    fn describe(&self) -> RuleInfo {
        RuleInfo {
            name: "anchors",
            summary: "Cross-references resolve, and the anchors they name are unique.",
            diagnostics: &[
                "anchor-without-id",
                "duplicate-anchor",
                "reference-without-id",
                "unresolved-reference",
            ],
            default_severity: Severity::Error,
            configured_by: Some("[anchors] declare_tag, reference_tag"),
        }
    }

    fn configured(&self, config: &Config) -> bool {
        config.anchors.declare_tag.is_some() || config.anchors.reference_tag.is_some()
    }

    fn check(&self, doc: &Document<'_>, corpus: &Corpus, out: &mut Vec<Diagnostic>) {
        check(doc, corpus, out);
    }
}

pub fn check(doc: &Document<'_>, index: &Corpus, out: &mut Vec<Diagnostic>) {
    let (rel, tree) = (doc.rel, doc.tree);
    let cfg = &index.config.anchors;
    let (declare, reference) = (cfg.declare_tag.as_deref(), cfg.reference_tag.as_deref());
    if declare.is_none() && reference.is_none() {
        return;
    }

    // An anchor declared twice in one file: the second wins silently, so
    // every reference to it lands on the wrong heading.
    let mut seen: HashMap<String, usize> = HashMap::new();

    walk(tree, &mut |node| {
        let name = tag_name(node);
        if name.is_some() && name == declare {
            let declare = declare.unwrap_or_default();
            let Some(id) = first_str_attr(node, &cfg.id_attributes).map(|s| s.trim().to_string())
            else {
                out.push(
                    Diagnostic::new(
                        rel,
                        Severity::Error,
                        "anchor-without-id",
                        format!(
                            "`{{% {declare} %}}` declares an anchor with no name, so nothing \
                             can reference it."
                        ),
                    )
                    .at(Span::of_node(node))
                    .help(format!("Write `{{% {declare} \"some_id\" /%}}`.")),
                );
                return;
            };
            let line = Span::of_node(node).map_or(0, |s| s.start_line);
            if let Some(first) = seen.get(&id) {
                out.push(
                    Diagnostic::new(
                        rel,
                        Severity::Error,
                        "duplicate-anchor",
                        format!("`{id}` is already declared on line {first} of this file."),
                    )
                    .at(Span::of_node(node))
                    .help(
                        "The later declaration wins, so every reference to this id lands on \
                         whichever heading happens to come last.",
                    ),
                );
            } else {
                seen.insert(id, line);
            }
        } else if name.is_some() && name == reference {
            let reference = reference.unwrap_or_default();
            // Cross-document, so it needs the index. Without one — an editor
            // opening a loose file — say nothing rather than reporting every
            // reference in the corpus as broken.
            if !index.is_indexed() {
                return;
            }
            let Some(id) = first_str_attr(node, &cfg.id_attributes) else {
                out.push(
                    Diagnostic::new(
                        rel,
                        Severity::Error,
                        "reference-without-id",
                        format!("`{{% {reference} %}}` names no anchor."),
                    )
                    .at(Span::of_node(node)),
                );
                return;
            };
            if index.anchor(rel, id).is_none() {
                let declare = declare.unwrap_or("tag");
                let scope = index.config.scope_of(rel);
                out.push(
                    Diagnostic::new(
                        rel,
                        Severity::Error,
                        "unresolved-reference",
                        format!("`{id}` is not an anchor this page can reach."),
                    )
                    .at(Span::of_node(node))
                    .help(format!(
                        "An anchor is `{{% {declare} \"{id}\" /%}}` on the target heading. \
                         This page sees the anchors of `{scope}`."
                    )),
                );
            }
        }
    });
}

#[cfg(test)]
mod tests {
    use crate::{Config, Corpus, lint_document};

    fn configured() -> Config {
        Config::parse("[anchors]\ndeclare_tag = \"tag\"\nreference_tag = \"tagref\"\n").unwrap()
    }

    #[test]
    fn an_anchor_declared_twice_is_reported() {
        let c = Corpus {
            config: configured(),
            ..Default::default()
        };
        let src = "# A {% tag \"x\" /%}\n\n# B {% tag \"x\" /%}\n";
        let found: Vec<_> = lint_document("a.md", src, &c)
            .diagnostics
            .into_iter()
            .filter(|d| d.rule == "duplicate-anchor")
            .collect();
        assert_eq!(found.len(), 1, "{found:?}");
        assert!(found[0].message.contains("line 1"), "{found:?}");
    }

    #[test]
    fn an_anchor_with_no_id_is_reported() {
        let c = Corpus {
            config: configured(),
            ..Default::default()
        };
        let rules: Vec<_> = lint_document("a.md", "# A {% tag /%}\n", &c)
            .diagnostics
            .into_iter()
            .map(|d| d.rule.into_owned())
            .collect();
        assert!(rules.iter().any(|r| r == "anchor-without-id"), "{rules:?}");
    }

    #[test]
    fn a_reference_says_nothing_without_an_index() {
        // An editor with one loose file open must not paint every
        // cross-reference red.
        let c = Corpus {
            config: configured(),
            ..Default::default()
        };
        let d = lint_document("a.md", "{% tagref \"nowhere\" /%}\n", &c).diagnostics;
        assert!(!d.iter().any(|d| d.rule == "unresolved-reference"), "{d:?}");
    }
}

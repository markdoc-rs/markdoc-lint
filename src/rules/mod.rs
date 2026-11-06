//! The rules, and the interface they all present.
//!
//! ## There used to be no trait here
//!
//! The earlier note in this file argued the opposite of what follows: six
//! free functions called from one place in `lint_document` is a list you can
//! read in ten seconds, and a trait buys dynamic dispatch nobody needs over
//! a set that changes once a year. That was right about dispatch and wrong
//! about what a trait is for here.
//!
//! What a list of function calls cannot do is *answer questions about
//! itself*. Every convention rule in this crate is off until a corpus
//! configures it — the module docs say so, one by one, "with neither named
//! this rule does nothing" — and the consequence is that a corpus with a
//! typo in `markdoc-lint.toml` and a corpus with nothing to report print
//! exactly the same thing: nothing. A writer cannot tell "clean" from "off",
//! and neither can CI.
//!
//! [`RuleInfo`] is what makes that answerable: a rule says its own name,
//! what it can emit, and which configuration turns it on, so a tool can list
//! the set and say *why* a rule is silent without knowing what any of them
//! do. The same description is what a rule loaded from somewhere else would
//! have to supply, which is the other reason to shape it now — RFC-0002,
//! "The rule interface".
//!
//! Dispatch is still not the point. [`ALL`] is a static slice of six, the
//! calls are devirtualised in a release build, and nothing measured changed.
//!
//! ## What a rule is
//!
//! Named for the thing it protects, not for the mechanism — `anchors`, not
//! `tagref_regex` — because the mechanism is the part most likely to be
//! replaced.
//!
//! `syntax` and `schema` are about Markdoc and always run. The other four
//! are about a project's conventions and return immediately unless
//! `markdoc-lint.toml` has configured them. A rule reports that state
//! through [`Rule::configured`] as well as acting on it, because the two
//! answers must not be allowed to drift apart.

pub mod anchors;
pub mod frontmatter;
pub mod hardcoded;
pub mod layout;
pub mod partials;
pub mod schema;
pub mod syntax;
pub mod variables;

use crate::{Config, Corpus, Diagnostic, Document, Severity};
use serde::{Deserialize, Serialize};

/// What a rule says about itself.
///
/// Every field is static: a description is about the rule, not about any
/// corpus, which is what lets a tool print the whole set before it has
/// found a content root.
#[derive(Debug, Clone, Serialize)]
pub struct RuleInfo {
    /// Stable, kebab-case, and the key a corpus writes under `[rules.…]`.
    /// It names the rule, not any one finding: `anchors` emits four
    /// different `Diagnostic.rule` ids.
    pub name: &'static str,
    /// One line, for a listing.
    pub summary: &'static str,
    /// Every `Diagnostic.rule` this rule can produce.
    ///
    /// Not decoration: it is the only place the two vocabularies are tied
    /// together, so somebody who greps a CI log for `unresolved-reference`
    /// can find out which rule to configure.
    pub diagnostics: &'static [&'static str],
    /// The strongest severity it emits with no override in force — which is
    /// the question a reader actually has, because it answers whether this
    /// rule can fail their commit. A rule that varies its severity per
    /// finding (`schema` does) reports the worst of them.
    pub default_severity: Severity,
    /// The configuration that turns it on, phrased as a reader would look
    /// for it in `markdoc-lint.toml`. `None` means the rule is about
    /// Markdoc itself and always runs.
    pub configured_by: Option<&'static str>,
}

/// What a corpus may re-decide about a rule.
///
/// Deliberately two knobs and not a policy language. `enabled = false` is
/// for a rule a corpus has not adopted yet; `severity` is for one it has
/// adopted but is not ready to be stopped by — the decision an
/// enforce-or-warn environment variable usually ends up carrying, said in
/// the place every other severity already lives.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct RuleSettings {
    pub enabled: Option<bool>,
    /// Applied to *every* finding the rule produces, replacing whatever the
    /// rule chose. A rule that emits both errors and warnings loses that
    /// distinction, which is the intended effect: the corpus has decided
    /// how much this rule matters and the rule no longer gets a vote.
    pub severity: Option<Severity>,
}

impl RuleSettings {
    pub fn is_enabled(&self) -> bool {
        self.enabled.unwrap_or(true)
    }
}

/// A check over one document.
///
/// `Sync` so the set can be a `static`, and because a caller linting a
/// corpus across threads holds the same rules on all of them.
pub trait Rule: Sync {
    fn describe(&self) -> RuleInfo;

    /// Whether this corpus has configured the rule.
    ///
    /// Reporting only — a rule still checks its own configuration before
    /// acting, because a rule that depended on the driver to gate it would
    /// behave differently when called directly.
    fn configured(&self, _config: &Config) -> bool {
        true
    }

    /// `doc.rel` is the corpus-relative path; several rules read it,
    /// because where a file sits decides whether it is a partial, which
    /// anchors it may reference and which frontmatter its type requires.
    /// Beside the tree, a [`Document`] carries the source and the tags the
    /// tokenizer recognised, which the tree cannot say: where a closing tag
    /// or an annotation was, and which tag a fault inside a paragraph left
    /// open.
    ///
    /// The whole [`Corpus`] rather than RFC-0002's `&Config`: four of the
    /// six rules are about whether something *elsewhere* exists, and the
    /// schema rule validates against the tag set the corpus assembled. A
    /// rule that could only see the configuration could not answer either.
    fn check(&self, doc: &Document<'_>, corpus: &Corpus, out: &mut Vec<Diagnostic>);
}

/// A check over the corpus rather than over a document.
///
/// One trait and not a variant of [`Rule`] because the inputs are genuinely
/// different: this is handed no tree, runs once rather than per file, and
/// answers questions — is this directory empty — that no document contains.
/// [`RuleInfo`] is shared, so both kinds list, configure and override the
/// same way, and a writer reading `tw check --list-rules` cannot tell which
/// list a rule came from, which is right: the difference is ours, not
/// theirs.
pub trait CorpusRule: Sync {
    fn describe(&self) -> RuleInfo;
    fn configured(&self, _config: &Config) -> bool {
        true
    }
    fn check(&self, corpus: &Corpus, out: &mut Vec<Diagnostic>);
}

pub static CORPUS_RULES: &[&dyn CorpusRule] = &[&layout::Layout];

/// Every rule, in the order they run.
///
/// Order is not arbitrary: `syntax` reports the one typo that would
/// otherwise be reported twice by `schema`, and suppressing a duplicate is
/// easier than un-reporting one.
pub static ALL: &[&dyn Rule] = &[
    &syntax::Syntax,
    &schema::Schema,
    &anchors::Anchors,
    &partials::Partials,
    &variables::Variables,
    &frontmatter::Frontmatter,
    &hardcoded::HardcodedVariables,
];

/// The rule set as it stands for one corpus: what each rule is, and whether
/// this configuration has it on, off, or re-levelled.
///
/// This is what `tw check --list-rules` prints. It exists here rather than
/// in a CLI so that every front end answers the question the same way.
#[derive(Debug, Clone, Serialize)]
pub struct RuleStatus {
    #[serde(flatten)]
    pub info: RuleInfo,
    /// Whether it will run: configured, and not switched off.
    pub active: bool,
    /// Whether the corpus named the configuration this rule needs.
    pub configured: bool,
    /// Set only when the corpus switched it off explicitly.
    pub disabled: bool,
    /// Set only when the corpus replaced the rule's severity.
    pub severity_override: Option<Severity>,
}

pub fn status(config: &Config) -> Vec<RuleStatus> {
    let document = ALL.iter().map(|r| (r.describe(), r.configured(config)));
    let corpus = CORPUS_RULES
        .iter()
        .map(|r| (r.describe(), r.configured(config)));
    document
        .chain(corpus)
        .map(|(info, configured)| {
            let settings = config.rule_settings(info.name);
            let disabled = settings.is_some_and(|s| !s.is_enabled());
            RuleStatus {
                info,
                active: configured && !disabled,
                configured,
                disabled,
                severity_override: settings.and_then(|s| s.severity),
            }
        })
        .collect()
}

/// The rule names, for validating `[rules.…]` keys.
pub fn names() -> Vec<&'static str> {
    ALL.iter()
        .map(|r| r.describe().name)
        .chain(CORPUS_RULES.iter().map(|r| r.describe().name))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_rule_has_a_distinct_name_and_says_what_it_can_emit() {
        // The name is a config key and the ids are what people grep for, so
        // a collision or an empty list is a rule nobody can address.
        let mut seen = std::collections::BTreeSet::new();
        for rule in ALL {
            let info = rule.describe();
            assert!(seen.insert(info.name), "duplicate rule name {}", info.name);
            assert!(!info.diagnostics.is_empty(), "{} emits nothing", info.name);
            assert!(
                info.name
                    .chars()
                    .all(|c| c.is_ascii_lowercase() || c == '-'),
                "{} is not kebab-case",
                info.name
            );
        }
    }

    #[test]
    fn an_unconfigured_corpus_has_two_active_rules() {
        // The upstream default: the two language rules, and nothing that
        // would fire on a corpus which never asked for it.
        let active: Vec<&str> = status(&Config::default())
            .iter()
            .filter(|s| s.active)
            .map(|s| s.info.name)
            .collect();
        assert_eq!(active, vec!["syntax", "schema"]);
    }

    #[test]
    fn a_rule_that_is_off_says_which_configuration_would_turn_it_on() {
        let off = status(&Config::default());
        let anchors = off.iter().find(|s| s.info.name == "anchors").unwrap();
        assert!(!anchors.configured);
        assert!(anchors.info.configured_by.unwrap().contains("declare_tag"));
    }
}

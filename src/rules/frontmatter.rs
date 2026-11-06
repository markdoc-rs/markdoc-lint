//! Frontmatter against the project's document-type rules.
//!
//! Every value here comes from `markdoc-lint.toml`: which key names the
//! type, which types exist, what each requires, and which fields are
//! enumerations. With no `[frontmatter]` section the rule does nothing,
//! because a document type system is a thing a project has and Markdoc does
//! not.

use crate::config::glob_match;
use crate::rules::{Rule, RuleInfo};
use crate::{Config, Corpus, Diagnostic, Document, Severity, Span};
use markdoc::IndexMap;
use markdoc::types::Scalar;
use serde::Deserialize;
use std::collections::BTreeMap;

/// A document's frontmatter, as markdoc reads YAML: in the file's order.
type Fields = IndexMap<String, Scalar>;

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct FrontmatterConfig {
    /// The key naming the document's type, or several, tried in order:
    /// `type_field = "type"`, or `type_field = ["doc_type", "type"]` for a
    /// corpus that writes both spellings. The first a document carries, as
    /// a string, is its type. Empty disables the type checks.
    ///
    /// A list because corpora drift between spellings, and one name sees
    /// only the documents that use it: told `type` while most of a corpus
    /// wrote `doc_type`, this rule checked one document in nine and said
    /// nothing about the rest.
    #[serde(deserialize_with = "one_or_many")]
    pub type_field: Vec<String>,
    /// Older spellings that still resolve: `release_note` -> `releaseNote`.
    pub aliases: BTreeMap<String, String>,
    /// Required of every typed document.
    pub required: Vec<String>,
    /// Allowed values for a field, for every type.
    pub enums: BTreeMap<String, Vec<String>>,
    /// Shapes a field's value must have, as regular expressions, for every
    /// type. A field with a pattern must also be a *string*: YAML reads
    /// `swVersion: 1.10` as the number 1.1, which is how an edition loses a
    /// digit between the file and the page.
    pub patterns: BTreeMap<String, String>,
    /// Per-type extra requirements.
    pub types: BTreeMap<String, TypeRule>,
    /// Requirements for files matching a path, typed or not.
    ///
    /// A type system keyed on a frontmatter field can only speak about
    /// documents that carry that field, and every corpus has some that do
    /// not — this one's `*/editions/*.md` carry a title and a version and
    /// nothing else. Without this, the only way to say anything about them
    /// is to give them a type, which then drags in every field the type
    /// system requires of a document.
    pub files: Vec<FileRule>,
}

/// Frontmatter requirements for the files a glob matches.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct FileRule {
    /// A corpus-relative path glob, as `[corpus] skip` uses.
    #[serde(rename = "match")]
    pub matches: String,
    pub required: Vec<String>,
    pub enums: BTreeMap<String, Vec<String>>,
    pub patterns: BTreeMap<String, String>,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct TypeRule {
    pub required: Vec<String>,
    /// Allowed values for a field, for this type only.
    pub enums: BTreeMap<String, Vec<String>>,
    /// Shapes a field's value must have, for this type only.
    pub patterns: BTreeMap<String, String>,
}

/// A key name, or a list of them, as a list.
fn one_or_many<'de, D>(d: D) -> Result<Vec<String>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    struct Visitor;
    impl<'de> serde::de::Visitor<'de> for Visitor {
        type Value = Vec<String>;
        fn expecting(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
            f.write_str("a key name, or a list of key names")
        }
        fn visit_str<E: serde::de::Error>(self, v: &str) -> Result<Self::Value, E> {
            Ok(if v.is_empty() {
                Vec::new()
            } else {
                vec![v.to_string()]
            })
        }
        fn visit_seq<A: serde::de::SeqAccess<'de>>(
            self,
            mut seq: A,
        ) -> Result<Self::Value, A::Error> {
            let mut out = Vec::new();
            while let Some(key) = seq.next_element::<String>()? {
                if !key.is_empty() {
                    out.push(key);
                }
            }
            Ok(out)
        }
    }
    d.deserialize_any(Visitor)
}

impl FrontmatterConfig {
    /// The first of [`Self::type_field`] the document carries, with what it
    /// says.
    pub fn type_of<'a>(&'a self, fm: &Fields) -> Option<(&'a str, String)> {
        self.type_field
            .iter()
            .find_map(|key| string_of(fm, key).map(|value| (key.as_str(), value)))
    }

    pub fn enabled(&self) -> bool {
        !self.type_field.is_empty() && !self.types.is_empty()
    }

    /// Every regular expression this configuration carries, with where it
    /// was written, so a bad one can be refused by name at load.
    pub fn every_pattern(&self) -> Vec<(String, &str)> {
        let mut out: Vec<(String, &str)> = self
            .patterns
            .iter()
            .map(|(f, p)| (format!("[frontmatter.patterns] {f}"), p.as_str()))
            .collect();
        for (name, rule) in &self.types {
            out.extend(rule.patterns.iter().map(|(f, p)| {
                (
                    format!("[frontmatter.types.{name}.patterns] {f}"),
                    p.as_str(),
                )
            }));
        }
        for rule in &self.files {
            out.extend(rule.patterns.iter().map(|(f, p)| {
                (
                    format!("[[frontmatter.files]] {} patterns.{f}", rule.matches),
                    p.as_str(),
                )
            }));
        }
        out
    }

    /// Resolve a written type to its canonical name.
    pub fn canonical<'a>(&'a self, raw: &str) -> Option<&'a str> {
        if let Some((name, _)) = self.types.get_key_value(raw) {
            return Some(name.as_str());
        }
        let aliased = self.aliases.get(raw)?;
        self.types
            .get_key_value(aliased.as_str())
            .map(|(k, _)| k.as_str())
    }
}

pub struct Frontmatter;

impl Rule for Frontmatter {
    fn describe(&self) -> RuleInfo {
        RuleInfo {
            name: "frontmatter",
            summary: "Each document's frontmatter carries what its type requires.",
            diagnostics: &[
                "unknown-doc-type",
                "missing-frontmatter-field",
                "invalid-frontmatter-value",
            ],
            default_severity: Severity::Error,
            configured_by: Some("[frontmatter] type_field + types, or [[frontmatter.files]]"),
        }
    }

    fn configured(&self, config: &Config) -> bool {
        config.frontmatter.enabled() || !config.frontmatter.files.is_empty()
    }

    fn check(&self, doc: &Document<'_>, corpus: &Corpus, out: &mut Vec<Diagnostic>) {
        check(doc, corpus, out);
    }
}

pub fn check(doc: &Document<'_>, index: &Corpus, out: &mut Vec<Diagnostic>) {
    let (rel, tree) = (doc.rel, doc.tree);
    let cfg = &index.config.frontmatter;
    if !cfg.enabled() && cfg.files.is_empty() {
        return;
    }
    // A partial is a fragment included into a page; it has no frontmatter of
    // its own and is not a document.
    if index.config.is_partial(rel) || index.config.is_skipped(rel) {
        return;
    }

    // The corpus is parsed with YAML frontmatter (`Corpus::parse_options`).
    // YAML that is not a mapping stays text, and `syntax` reports it as
    // `unparsable`.
    let Some(Scalar::Object(fm)) = tree.attributes.get("frontmatter") else {
        // No frontmatter at all. Pages that inherit it from a parent
        // document legitimately have none, and deciding which those are
        // needs the navigation, which is a corpus question rather than a
        // document one.
        return;
    };

    // Path rules first, and whether or not the document is typed.
    for rule in &cfg.files {
        if !glob_match(&rule.matches, rel) {
            continue;
        }
        let scope = format!("`{}`", rule.matches);
        apply(
            rel,
            fm,
            &rule.required.iter().collect::<Vec<_>>(),
            rule.enums.iter().collect::<Vec<_>>(),
            rule.patterns.iter().collect::<Vec<_>>(),
            &scope,
            out,
        );
    }

    if !cfg.enabled() {
        return;
    }

    let Some((type_key, doc_type)) = cfg.type_of(fm) else {
        return;
    };

    let Some(canonical) = cfg.canonical(&doc_type) else {
        let mut known: Vec<&str> = cfg.types.keys().map(|s| s.as_str()).collect();
        known.sort_unstable();
        out.push(
            Diagnostic::new(
                rel,
                Severity::Error,
                "unknown-doc-type",
                format!("`{type_key}: {doc_type}` is not a document type."),
            )
            .at(Some(Span::line(1)))
            .help(format!("One of: {}.", known.join(", "))),
        );
        return;
    };

    let rule = &cfg.types[canonical];
    apply(
        rel,
        fm,
        &cfg.required
            .iter()
            .chain(rule.required.iter())
            .collect::<Vec<_>>(),
        cfg.enums
            .iter()
            .chain(rule.enums.iter())
            .collect::<Vec<_>>(),
        cfg.patterns
            .iter()
            .chain(rule.patterns.iter())
            .collect::<Vec<_>>(),
        &format!("a `{canonical}`"),
        out,
    );
}

/// One set of requirements against one document's frontmatter.
///
/// `scope` is how the finding names what asked -- a type, or the glob that
/// matched -- because a writer looking at `missing-frontmatter-field` needs
/// to know which rule to go and read.
fn apply(
    rel: &str,
    fm: &Fields,
    required: &[&String],
    enums: Vec<(&String, &Vec<String>)>,
    patterns: Vec<(&String, &String)>,
    scope: &str,
    out: &mut Vec<Diagnostic>,
) {
    for field in required {
        if !fm.contains_key(field.as_str()) {
            out.push(
                Diagnostic::new(
                    rel,
                    Severity::Error,
                    "missing-frontmatter-field",
                    format!("`{field}` is required for {scope}."),
                )
                .at(Some(Span::line(1))),
            );
        }
    }

    for (field, allowed) in enums {
        let Some(value) = string_of(fm, field) else {
            continue;
        };
        if !allowed.contains(&value) {
            out.push(
                Diagnostic::new(
                    rel,
                    Severity::Error,
                    "invalid-frontmatter-value",
                    format!("`{field}: {value}` is not an allowed value."),
                )
                .at(Some(Span::line(1)))
                .help(format!("One of: {}.", allowed.join(", "))),
            );
        }
    }

    for (field, pattern) in patterns {
        let Some(raw) = fm.get(field.as_str()) else {
            continue;
        };
        // Unquoted first, and said differently, because the cause is
        // different. `swVersion: 1.10` is not a typo -- it is YAML reading
        // a version as a number, and 1.10 and 1.1 are the same number.
        let Scalar::String(value) = raw else {
            if matches!(raw, Scalar::Null) {
                continue;
            }
            out.push(
                Diagnostic::new(
                    rel,
                    Severity::Error,
                    "invalid-frontmatter-value",
                    format!(
                        "`{field}` must be a quoted string here, and is {}.",
                        kind(raw)
                    ),
                )
                .at(Some(Span::line(1)))
                // Deliberately not echoing the value back as the thing to
                // quote. By the time this rule sees it the damage is done:
                // `1.10` is already the number 1.1, and telling the writer
                // to quote `"3.1"` when they typed `3.10` would hand them
                // the bug back in a fixed-looking form.
                .help(
                    "Quote it. Unquoted, YAML reads a version as a number, so `1.10` \
                     and `1.1` are the same value -- which is also why what this \
                     finding can see may not be what the file says."
                        .to_string(),
                ),
            );
            continue;
        };
        // Compiled here rather than cached: one small expression per
        // document is tens of microseconds against a parse, and `parse`
        // already refused a configuration whose patterns do not compile.
        let Ok(re) = regex::Regex::new(pattern) else {
            continue;
        };
        if !re.is_match(value.trim()) {
            out.push(
                Diagnostic::new(
                    rel,
                    Severity::Error,
                    "invalid-frontmatter-value",
                    format!("`{field}: {value}` is not the shape {scope} requires."),
                )
                .at(Some(Span::line(1)))
                .help(format!("It has to match `{pattern}`.")),
            );
        }
    }
}

fn kind(value: &Scalar) -> &'static str {
    match value {
        Scalar::Undefined | Scalar::Null => "empty",
        Scalar::Boolean(_) => "a boolean",
        Scalar::Number(_) => "a number",
        Scalar::String(_) => "a string",
        Scalar::Array(_) => "a list",
        Scalar::Object(_) => "a map",
        // YAML never makes these; markdoc's tag values can.
        Scalar::Variable(_) | Scalar::Function(_) => "an expression",
        Scalar::Tag(_) => "a tag",
        Scalar::Js(_) => "an object",
    }
}

fn string_of(fm: &Fields, key: &str) -> Option<String> {
    match fm.get(key) {
        Some(Scalar::String(s)) if !s.trim().is_empty() => Some(s.trim().to_string()),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use crate::{Config, Corpus, lint_document};

    fn corpus() -> Corpus {
        let config = Config::parse(
            r#"
            [partials]
            dir = "partials"

            [frontmatter]
            type_field = "type"
            required = ["id", "title", "status"]
            aliases = { release_note = "releaseNote" }
            enums = { status = ["draft", "published"] }

            [frontmatter.types.article]

            [frontmatter.types.faq]
            required = ["question"]

            [frontmatter.types.releaseNote]
            required = ["swVersion"]

            [frontmatter.types.notice]
            enums = { category = ["safety", "product"] }
            "#,
        )
        .unwrap();
        Corpus {
            config,
            ..Default::default()
        }
    }

    fn lint(rel: &str, src: &str) -> Vec<String> {
        lint_document(rel, src, &corpus())
            .diagnostics
            .into_iter()
            .map(|d| format!("{}: {}", d.rule, d.message))
            .collect()
    }

    /// A corpus whose editions are untyped, as this one's are: a title and
    /// a version, and none of the eight fields a document needs.
    fn editions() -> Corpus {
        let config = Config::parse(
            r#"
            [[frontmatter.files]]
            match = "*/editions/*.md"
            required = ["title", "swVersion"]
            patterns = { swVersion = '^[0-9]+\.[0-9]+(\.[0-9]+)?$' }
            "#,
        )
        .unwrap();
        Corpus {
            config,
            ..Default::default()
        }
    }

    fn lint_in(corpus: &Corpus, rel: &str, src: &str) -> Vec<String> {
        lint_document(rel, src, corpus)
            .diagnostics
            .into_iter()
            .map(|d| format!("{}: {}", d.rule, d.message))
            .collect()
    }

    #[test]
    fn an_unquoted_version_is_a_number_and_the_finding_says_why() {
        // The one that matters: YAML reads `1.10` as 1.1, so the edition
        // silently becomes a different one. A "does not match the pattern"
        // message would send the writer looking for a typo.
        let found = lint_in(
            &editions(),
            "documentation/editions/x.md",
            "---\ntitle: T\nswVersion: 1.10\n---\n\nBody.\n",
        );
        assert_eq!(found.len(), 1, "{found:?}");
        assert!(found[0].contains("must be a quoted string"), "{found:?}");
    }

    #[test]
    fn a_quoted_version_of_the_right_shape_says_nothing() {
        assert!(
            lint_in(
                &editions(),
                "documentation/editions/x.md",
                "---\ntitle: T\nswVersion: \"3.10\"\n---\n\nBody.\n",
            )
            .is_empty()
        );
    }

    #[test]
    fn a_quoted_version_of_the_wrong_shape_is_reported() {
        let found = lint_in(
            &editions(),
            "documentation/editions/x.md",
            "---\ntitle: T\nswVersion: \"2.11.0 and higher\"\n---\n\nBody.\n",
        );
        assert_eq!(found.len(), 1, "{found:?}");
        assert!(found[0].contains("is not the shape"), "{found:?}");
    }

    #[test]
    fn a_path_rule_reaches_a_document_no_type_system_can() {
        // No `type:` anywhere, and the rule still applies -- which is the
        // whole reason this scope exists.
        let found = lint_in(
            &editions(),
            "documentation/editions/x.md",
            "---\ntitle: T\n---\n\nBody.\n",
        );
        assert!(
            found.iter().any(|d| d.contains("`swVersion` is required")),
            "{found:?}"
        );
        // And not to a document the glob does not match.
        assert!(
            lint_in(
                &editions(),
                "documentation/other.md",
                "---\ntitle: T\n---\n\nB.\n"
            )
            .is_empty()
        );
    }

    #[test]
    fn a_pattern_that_does_not_compile_is_refused_at_load() {
        let err =
            Config::parse("[[frontmatter.files]]\nmatch = \"a.md\"\npatterns = { x = \"([\" }\n")
                .unwrap_err();
        assert!(err.contains("not a valid pattern"), "{err}");
        assert!(err.contains("frontmatter.files"), "{err}");
    }

    #[test]
    fn a_type_must_carry_its_own_extra_fields() {
        let found = lint(
            "a.md",
            "---\ntype: faq\nid: x\ntitle: T\nstatus: published\n---\n\n# T\n",
        );
        assert!(
            found.iter().any(|d| d.contains("`question` is required")),
            "{found:?}"
        );
    }

    #[test]
    fn a_complete_document_says_nothing() {
        assert_eq!(
            lint(
                "a.md",
                "---\ntype: article\nid: x\ntitle: T\nstatus: published\n---\n\n# T\n"
            ),
            Vec::<String>::new()
        );
    }

    #[test]
    fn an_enum_is_checked_globally_and_per_type() {
        let found = lint(
            "a.md",
            "---\ntype: article\nid: x\ntitle: T\nstatus: finished\n---\n\n# T\n",
        );
        assert!(
            found.iter().any(|d| d.contains("`status: finished`")),
            "{found:?}"
        );
        // `category` is only an enum for a notice.
        let ok = lint(
            "a.md",
            "---\ntype: article\nid: x\ntitle: T\nstatus: draft\ncategory: anything\n---\n\n# T\n",
        );
        assert_eq!(ok, Vec::<String>::new(), "{ok:?}");
        let bad = lint(
            "a.md",
            "---\ntype: notice\nid: x\ntitle: T\nstatus: draft\ncategory: anything\n---\n\n# T\n",
        );
        assert!(
            bad.iter().any(|d| d.contains("`category: anything`")),
            "{bad:?}"
        );
    }

    #[test]
    fn an_old_spelling_still_resolves() {
        let found = lint(
            "a.md",
            "---\ntype: release_note\nid: x\ntitle: T\nstatus: draft\n---\n\n# T\n",
        );
        assert!(
            found.iter().any(|d| d.contains("`swVersion` is required")),
            "{found:?}"
        );
        assert!(
            !found.iter().any(|d| d.contains("not a document type")),
            "{found:?}"
        );
    }

    #[test]
    fn a_partial_is_not_a_document_and_is_left_alone() {
        assert_eq!(
            lint(
                "partials/common/text_x.md",
                "---\ntype: article\n---\n\nx.\n"
            ),
            Vec::<String>::new()
        );
    }

    #[test]
    fn with_no_frontmatter_section_the_rule_is_silent() {
        let d = lint_document(
            "a.md",
            "---\ntype: nonsense\n---\n\n# T\n",
            &Corpus::empty(),
        )
        .diagnostics;
        assert!(d.is_empty(), "{d:?}");
    }

    /// A corpus that writes the type under two names, as one does.
    fn two_spellings() -> Corpus {
        let config = Config::parse(
            r#"
            [frontmatter]
            type_field = ["doc_type", "type"]
            required = ["title"]
            [frontmatter.types.article]
            "#,
        )
        .unwrap();
        Corpus {
            config,
            ..Default::default()
        }
    }

    #[test]
    fn every_spelling_of_the_type_field_is_checked() {
        let c = two_spellings();
        for key in ["doc_type", "type"] {
            let found = lint_in(&c, "a.md", &format!("---\n{key}: article\n---\n\nBody.\n"));
            assert_eq!(
                found,
                vec!["missing-frontmatter-field: `title` is required for a `article`."],
                "{key}"
            );
            let found = lint_in(
                &c,
                "a.md",
                &format!("---\n{key}: articel\ntitle: T\n---\n\nBody.\n"),
            );
            assert_eq!(found.len(), 1, "{key}: {found:?}");
            assert!(found[0].contains(&format!("`{key}: articel`")), "{found:?}");
        }
        // The first one present decides.
        let found = lint_in(
            &c,
            "a.md",
            "---\ndoc_type: article\ntype: nonsense\ntitle: T\n---\n\nBody.\n",
        );
        assert!(found.is_empty(), "{found:?}");
    }

    #[test]
    fn a_type_field_that_is_not_a_name_is_refused_at_load() {
        let err = Config::parse("[frontmatter]\ntype_field = 3\n").unwrap_err();
        assert!(err.contains("a key name, or a list of key names"), "{err}");
        let c = Config::parse("[frontmatter]\ntype_field = \"\"\n[frontmatter.types.article]\n")
            .unwrap();
        assert!(!c.frontmatter.enabled(), "an empty name is no type field");
    }
}

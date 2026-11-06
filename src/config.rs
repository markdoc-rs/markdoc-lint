//! The conventions a project has, which Markdoc itself has no opinion about.
//!
//! ## Why this file exists at all
//!
//! Every rule in this crate except two is about a convention rather than
//! about Markdoc. `{% tagref "x" %}` is a perfectly valid tag whatever `x`
//! is; that it must name an anchor declared by `{% tag "x" /%}` somewhere a
//! page can reach is a thing *one corpus* decided. The same goes for which
//! directory partials live in, what their filenames begin with, where
//! variables come from and which frontmatter fields a document type
//! requires.
//!
//! Hard-coding those is how a linter becomes un-adoptable: the rules are
//! useful, the values are not transferable, and a second project forks the
//! crate to change nine string literals. So the values are data.
//!
//! ## Each rule's section belongs to the rule
//!
//! This file used to define all of them, which made adding a rule an edit
//! to a file the rule has nothing to do with, and made `Config` the one
//! place that had to know every rule's vocabulary. The section types now
//! live beside the rules that read them — [`AnchorsConfig`] in
//! `rules::anchors`, and so on — and are re-exported here, so a rule brings
//! its own configuration and this file only assembles them. The sections
//! stay typed and `deny_unknown_fields` still catches a misspelled key: a
//! typo that parses is a rule that stops firing with nobody noticing.
//!
//! `[rules.…]` is the one table keyed by rule name rather than by section.
//! It is where a corpus overrides a rule's severity or switches it off, and
//! its keys are validated against the registry, so a plugin rule would get
//! the same treatment without this file learning its name.
//!
//! ## The default is "say nothing"
//!
//! With no `markdoc-lint.toml` the convention rules are off and only
//! `syntax` and `schema` run — the two that are about the language. That is
//! deliberate: a rule which fires on every file of a corpus it was not
//! configured for teaches people to pass `--no-verify`, and an editor that
//! opens one loose `.md` has no project to read conventions from.
//!
//! The configuration is read once, by the CLI from the corpus root and by
//! the language server when it finds the root; the rules themselves never
//! touch the filesystem.

use crate::rules::{self, RuleSettings};
use serde::Deserialize;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

pub use crate::rules::anchors::{AnchorsConfig, GLOBAL_SCOPE, Scope};
pub use crate::rules::frontmatter::{FrontmatterConfig, TypeRule};
pub use crate::rules::hardcoded::HardcodedConfig;
pub use crate::rules::layout::LayoutRule;
pub use crate::rules::partials::PartialsConfig;
pub use crate::rules::schema::SchemaConfig;
pub use crate::rules::variables::VariablesConfig;

/// The file a project puts at its content root.
pub const CONFIG_FILENAME: &str = "markdoc-lint.toml";

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct Config {
    pub corpus: CorpusConfig,
    pub partials: PartialsConfig,
    pub anchors: AnchorsConfig,
    pub variables: VariablesConfig,
    pub frontmatter: FrontmatterConfig,
    pub schema: SchemaConfig,
    /// Spelled with a hyphen in the file, to match the rule's name.
    #[serde(rename = "hardcoded-variables")]
    pub hardcoded: HardcodedConfig,
    /// What a directory must contain. A list, because a corpus has several
    /// such conventions and they are about different directories.
    pub layout: Vec<LayoutRule>,
    /// Overrides, keyed by rule name rather than by section — see
    /// [`rules::RuleSettings`].
    pub rules: BTreeMap<String, RuleSettings>,
}

impl Config {
    /// Read `markdoc-lint.toml` from a directory, or the built-in default if
    /// there is none.
    ///
    /// A *malformed* file is a different thing from a missing one and is
    /// returned as an error: silently falling back to the defaults would
    /// turn a typo in the config into "the linter stopped finding things",
    /// which is the failure mode hardest to notice.
    pub fn load(dir: &Path) -> Result<Self, ConfigError> {
        let path = dir.join(CONFIG_FILENAME);
        match std::fs::read_to_string(&path) {
            Ok(text) => Self::parse(&text).map_err(|e| ConfigError {
                path: Some(path),
                message: e,
            }),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Self::default()),
            Err(e) => Err(ConfigError {
                path: Some(path),
                message: e.to_string(),
            }),
        }
    }

    pub fn parse(text: &str) -> Result<Self, String> {
        let config: Self = toml::from_str(text).map_err(|e| e.to_string())?;
        config.check_rule_names()?;
        config.check_patterns()?;
        Ok(config)
    }

    /// `[rules.nonsense]` is a typo, not an extension point.
    ///
    /// `deny_unknown_fields` cannot see this one: the table is a map, so
    /// every key is a valid key as far as serde is concerned. Checking it
    /// against the registry is what keeps a misspelled override from being
    /// a silently ignored one — the same failure `deny_unknown_fields`
    /// exists to prevent, one level down.
    fn check_rule_names(&self) -> Result<(), String> {
        let known = rules::names();
        for name in self.rules.keys() {
            if !known.contains(&name.as_str()) {
                return Err(format!(
                    "[rules.{name}] is not a rule. Known rules: {}",
                    known.join(", ")
                ));
            }
        }
        Ok(())
    }

    /// A frontmatter `pattern` that does not compile is refused here rather
    /// than skipped at check time, for the same reason a misspelled key is:
    /// the alternative is a rule that silently stopped looking.
    fn check_patterns(&self) -> Result<(), String> {
        for (where_, pattern) in self.frontmatter.every_pattern() {
            regex::Regex::new(pattern)
                .map_err(|e| format!("{where_}: {pattern} is not a valid pattern: {e}"))?;
        }
        Ok(())
    }

    /// What this corpus re-decided about a rule, if anything.
    pub fn rule_settings(&self, name: &str) -> Option<&RuleSettings> {
        self.rules.get(name)
    }

    /// Is this a file the project does not want checked?
    pub fn is_skipped(&self, rel: &str) -> bool {
        self.corpus.skip.iter().any(|p| glob_match(p, rel))
    }

    /// Is this file a partial — a fragment included into pages, which has no
    /// frontmatter and declares no anchors of its own?
    pub fn is_partial(&self, rel: &str) -> bool {
        self.partials.is_partial(rel)
    }

    /// Which anchors a file may reference.
    pub fn scope_of(&self, rel: &str) -> String {
        self.anchors.scope_of(rel)
    }
}

#[derive(Debug)]
pub struct ConfigError {
    pub path: Option<PathBuf>,
    pub message: String,
}

impl std::fmt::Display for ConfigError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match &self.path {
            Some(p) => write!(f, "{}: {}", p.display(), self.message),
            None => write!(f, "{}", self.message),
        }
    }
}

impl std::error::Error for ConfigError {}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct CorpusConfig {
    /// Paths the checks do not apply to. `*` matches within a path segment
    /// and `**` across segments.
    pub skip: Vec<String>,
}

impl Default for CorpusConfig {
    fn default() -> Self {
        Self {
            skip: vec![
                "node_modules/**".to_string(),
                ".*".to_string(),
                ".*/**".to_string(),
            ],
        }
    }
}

/// A path glob: `*` matches within one segment, `**` across segments.
///
/// Hand-rolled rather than pulled from a crate because the whole of the need
/// is three forms — a literal, a prefix and a suffix — and a glob dependency
/// in a language server is a dependency a writer installs.
pub fn glob_match(pattern: &str, path: &str) -> bool {
    glob_inner(pattern.as_bytes(), path.as_bytes())
}

fn glob_inner(pat: &[u8], s: &[u8]) -> bool {
    if pat.is_empty() {
        return s.is_empty();
    }
    if pat.starts_with(b"**") {
        let rest = &pat[2..];
        // `**/` also matches zero directories, so `a/**/b` matches `a/b`.
        let rest = rest.strip_prefix(b"/").unwrap_or(rest);
        if glob_inner(rest, s) {
            return true;
        }
        for i in 0..=s.len() {
            if glob_inner(rest, &s[i..]) {
                return true;
            }
        }
        return false;
    }
    if pat[0] == b'*' {
        let rest = &pat[1..];
        let limit = s.iter().position(|c| *c == b'/').unwrap_or(s.len());
        for i in 0..=limit {
            if glob_inner(rest, &s[i..]) {
                return true;
            }
        }
        return false;
    }
    if !s.is_empty() && pat[0] == s[0] {
        return glob_inner(&pat[1..], &s[1..]);
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_default_config_turns_the_convention_rules_off() {
        let c = Config::default();
        assert!(c.anchors.declare_tag.is_none());
        assert!(c.partials.dir.is_none());
        assert!(c.variables.sources.is_empty());
        assert!(!c.frontmatter.enabled());
    }

    #[test]
    fn globs_match_within_and_across_segments() {
        assert!(glob_match("node_modules/**", "node_modules/a/b.md"));
        assert!(glob_match("*_config.json", "robot_config.json"));
        assert!(!glob_match("*_config.json", "a/robot_config.json"));
        assert!(glob_match("**/*_config.json", "a/b/robot_config.json"));
        assert!(glob_match(".tooling/**", ".tooling/scripts/x.ts"));
        assert!(glob_match("README.md", "README.md"));
        assert!(!glob_match("README.md", "docs/README.md"));
        // `**` matching zero segments.
        assert!(glob_match("a/**/b.md", "a/b.md"));
    }

    #[test]
    fn a_path_prefix_scope_is_the_prefix_plus_n_segments() {
        let c: Config = Config::parse(
            r#"
            [anchors]
            declare_tag = "tag"
            scope = { kind = "path-prefix", under = "products", depth = 1 }
            "#,
        )
        .unwrap();
        assert_eq!(c.scope_of("products/widget/manual/a.md"), "products/widget");
        assert_eq!(c.scope_of("products/gadget/x/y/z.md"), "products/gadget");
        // Outside the prefix, and the prefix with nothing under it.
        assert_eq!(c.scope_of("documentation/a.md"), GLOBAL_SCOPE);
        assert_eq!(c.scope_of("products"), GLOBAL_SCOPE);
    }

    #[test]
    fn a_global_scope_puts_everything_in_one_namespace() {
        let c = Config::default();
        assert_eq!(c.scope_of("products/widget/a.md"), GLOBAL_SCOPE);
        assert_eq!(c.scope_of("anything.md"), GLOBAL_SCOPE);
    }

    #[test]
    fn an_unknown_key_is_an_error_rather_than_a_silent_no_op() {
        // A typo in a config that still parses is a rule that stops firing
        // without anyone noticing.
        let err = Config::parse("[anchors]\ndeclare_tagg = \"tag\"\n").unwrap_err();
        assert!(err.contains("declare_tagg"), "{err}");
    }

    #[test]
    fn an_override_of_a_rule_that_does_not_exist_is_an_error_too() {
        // The same failure one level down: serde sees a map and accepts any
        // key, so the registry has to be the one to object.
        let err = Config::parse("[rules.anchor]\nenabled = false\n").unwrap_err();
        assert!(err.contains("[rules.anchor] is not a rule"), "{err}");
        assert!(err.contains("anchors"), "{err}");
        // And the correctly spelled one is accepted.
        let c = Config::parse("[rules.anchors]\nenabled = false\n").unwrap();
        assert!(!c.rule_settings("anchors").unwrap().is_enabled());
    }

    #[test]
    fn aliases_resolve_to_the_canonical_type() {
        let c = Config::parse(
            r#"
            [frontmatter]
            type_field = "type"
            aliases = { release_note = "releaseNote" }
            [frontmatter.types.releaseNote]
            required = ["swVersion"]
            "#,
        )
        .unwrap();
        assert_eq!(c.frontmatter.canonical("release_note"), Some("releaseNote"));
        assert_eq!(c.frontmatter.canonical("releaseNote"), Some("releaseNote"));
        assert_eq!(c.frontmatter.canonical("nonsense"), None);
    }
}

//! `{% partial file="…" /%}` — does the file exist, and is it named like a
//! partial.
//!
//! The naming check is house style and is off unless `prefixes` is set: a
//! partial's name saying what kind of block it is is what makes a directory
//! of four hundred of them navigable, but that is a convention, not a fact
//! about Markdoc.

use crate::rules::{Rule, RuleInfo};
use crate::{Config, Corpus, Diagnostic, Document, Severity, Span, first_str_attr, tag_name, walk};
use serde::Deserialize;

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct PartialsConfig {
    /// The directory partial paths are written relative to. `None` disables
    /// the partial rules entirely.
    pub dir: Option<String>,
    /// The tag that includes one.
    pub tag: String,
    /// Attributes that may carry the path, in the order they are tried.
    pub file_attributes: Vec<String>,
    /// What a partial's filename may begin with. Empty disables the naming
    /// check, which is house style rather than correctness.
    pub prefixes: Vec<String>,
}

impl Default for PartialsConfig {
    fn default() -> Self {
        Self {
            dir: None,
            tag: "partial".to_string(),
            file_attributes: vec!["file".to_string(), "primary".to_string()],
            prefixes: Vec::new(),
        }
    }
}

impl PartialsConfig {
    /// Is this file a fragment included into pages, rather than a page?
    ///
    /// Read by three other rules, not only this one: a partial has no
    /// frontmatter to check and declares no anchors of its own, so knowing
    /// what a partial *is* is a thing the partials configuration owns and
    /// the rest of the crate asks about.
    pub fn is_partial(&self, rel: &str) -> bool {
        match &self.dir {
            None => false,
            Some(dir) => rel == dir || rel.starts_with(&format!("{dir}/")),
        }
    }
}

pub struct Partials;

impl Rule for Partials {
    fn describe(&self) -> RuleInfo {
        RuleInfo {
            name: "partials",
            summary: "Every `{% partial %}` names a file that exists, and is named like one.",
            diagnostics: &["partial-without-file", "missing-partial", "partial-naming"],
            default_severity: Severity::Error,
            configured_by: Some("[partials] dir"),
        }
    }

    fn configured(&self, config: &Config) -> bool {
        config.partials.dir.is_some()
    }

    fn check(&self, doc: &Document<'_>, corpus: &Corpus, out: &mut Vec<Diagnostic>) {
        check(doc, corpus, out);
    }
}

pub fn check(doc: &Document<'_>, index: &Corpus, out: &mut Vec<Diagnostic>) {
    let (rel, tree) = (doc.rel, doc.tree);
    let cfg = &index.config.partials;
    let Some(dir) = cfg.dir.as_deref() else {
        return;
    };

    walk(tree, &mut |node| {
        if tag_name(node) != Some(cfg.tag.as_str()) {
            return;
        }
        let Some(file) = first_str_attr(node, &cfg.file_attributes) else {
            out.push(
                Diagnostic::new(
                    rel,
                    Severity::Error,
                    "partial-without-file",
                    format!("`{{% {} %}}` names no file.", cfg.tag),
                )
                .at(Span::of_node(node))
                .help(format!("Write `{{% {} file=\"…/name.md\" /%}}`.", cfg.tag)),
            );
            return;
        };
        let file = file.trim();

        // Partial paths are written relative to the partials directory, so
        // that is how they are resolved.
        let target = format!("{dir}/{}", file.trim_start_matches('/'));

        if index.is_indexed() && !index.partials.contains(&target) {
            out.push(
                Diagnostic::new(
                    rel,
                    Severity::Error,
                    "missing-partial",
                    format!("`{file}` is not a file under `{dir}/`."),
                )
                .at(Span::of_node(node))
                .help(format!("Looked for `{target}`.")),
            );
            return;
        }

        if cfg.prefixes.is_empty() {
            return;
        }
        let base = file.rsplit('/').next().unwrap_or(file);
        if !cfg.prefixes.iter().any(|p| base.starts_with(p)) {
            out.push(
                Diagnostic::new(
                    rel,
                    Severity::Warning,
                    "partial-naming",
                    format!("`{base}` does not start with a partial prefix."),
                )
                .at(Span::of_node(node))
                .help(format!("One of: {}.", cfg.prefixes.join(", "))),
            );
        }
    });
}

#[cfg(test)]
mod tests {
    use crate::{Config, Corpus, lint_document};

    #[test]
    fn a_doubled_slash_is_a_missing_partial() {
        let config =
            Config::parse("[partials]\ndir = \"partials\"\nprefixes = [\"text\"]\n").unwrap();
        let mut c = Corpus {
            config,
            files: 1,
            ..Default::default()
        };
        c.partials.insert("partials/common/text_a.md".to_string());
        let d =
            lint_document("a.md", "{% partial file=\"common//text_a.md\" /%}\n", &c).diagnostics;
        assert!(d.iter().any(|d| d.rule == "missing-partial"), "{d:?}");
    }

    #[test]
    fn the_naming_check_is_off_unless_prefixes_are_configured() {
        let config = Config::parse("[partials]\ndir = \"partials\"\n").unwrap();
        let mut c = Corpus {
            config,
            files: 1,
            ..Default::default()
        };
        c.partials.insert("partials/anything.md".to_string());
        let d = lint_document("a.md", "{% partial file=\"anything.md\" /%}\n", &c).diagnostics;
        assert!(d.is_empty(), "{d:?}");
    }

    #[test]
    fn with_no_partials_dir_the_rule_is_silent() {
        let c = Corpus::empty();
        let d = lint_document("a.md", "{% partial file=\"nowhere.md\" /%}\n", &c).diagnostics;
        assert!(!d.iter().any(|d| d.rule == "missing-partial"), "{d:?}");
    }
}

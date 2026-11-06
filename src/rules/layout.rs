//! What a directory must contain.
//!
//! The only rule here that is not about a document. Every other rule in this
//! crate is handed one file's tree and asked a question about it; this one
//! is handed the corpus and asked about its shape — an `editions/` directory
//! with no editions in it, a product variant with a manual and no navigation
//! file beside it. Neither fault is visible in any single document, which is
//! why neither was ever found by the thing that reads documents.
//!
//! ## Why it is config and not code
//!
//! The two checks this replaces are 75 lines of TypeScript that between them
//! name `editions`, `products`, `translations`, `styles`, `tools`, the `_`
//! prefix, `*_manual.md` and `*_nav.json`. Every one of those is this
//! corpus's, and none of them is a fact about Markdoc. What is left when
//! they are taken out is two sentences — *this kind of directory must hold
//! one of these*, and *a directory holding one of these must also hold one
//! of those* — which is the whole of the configuration below.
//!
//! ## The glob is the one already here
//!
//! `config::glob_match`, unchanged, and its semantics answer both halves.
//! A pattern matches the **whole** corpus-relative path, so it is anchored
//! at the root: `*/editions` is a top-level collection's editions and not
//! `a/b/editions`, which is what the TypeScript achieved by reading only the
//! top level. `*` does not cross a `/` and `**` matches zero or more
//! segments, so `require` says its own recursion — `*.md` is direct
//! children, `**/*.md` is the subtree — and there is no `recursive` flag and
//! one dialect in the file instead of two.

use crate::config::glob_match;
use crate::corpus::find_dirs;
use crate::rules::{CorpusRule, RuleInfo};
use crate::{Config, Corpus, Diagnostic, Severity};
use serde::Deserialize;

/// One assertion about a directory.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct LayoutRule {
    /// Which directories this is about, as a corpus-relative path glob.
    pub dir: String,
    /// Only directories that already hold a file matching this. Absent
    /// means every directory the `dir` glob matched.
    ///
    /// This is the difference between "a variant must have navigation" —
    /// which is false, most do not — and "a variant that has a manual must
    /// have navigation", which is the real convention.
    pub when_contains: Option<String>,
    /// What at least one file in the directory must match. Paths are
    /// matched relative to the directory, so the glob says its own depth.
    pub require: String,
}

pub struct Layout;

impl CorpusRule for Layout {
    fn describe(&self) -> RuleInfo {
        RuleInfo {
            name: "layout",
            summary: "A directory holds the files the corpus says it holds.",
            diagnostics: &["missing-file"],
            default_severity: Severity::Error,
            configured_by: Some("[[layout]]"),
        }
    }

    fn configured(&self, config: &Config) -> bool {
        !config.layout.is_empty()
    }

    fn check(&self, corpus: &Corpus, out: &mut Vec<Diagnostic>) {
        check(corpus, out);
    }
}

pub fn check(corpus: &Corpus, out: &mut Vec<Diagnostic>) {
    let rules = &corpus.config.layout;
    if rules.is_empty() {
        return;
    }

    for dir in find_dirs(&corpus.root) {
        let Ok(rel) = dir.strip_prefix(&corpus.root) else {
            continue;
        };
        let rel = rel.to_string_lossy().replace('\\', "/");
        if rel.is_empty() || corpus.config.is_skipped(&rel) {
            continue;
        }

        let mut contents: Option<Vec<String>> = None;
        for rule in rules {
            if rule.require.is_empty() || !glob_match(&rule.dir, &rel) {
                continue;
            }
            // Read once, and only for a directory some rule is about.
            let files = contents.get_or_insert_with(|| {
                crate::corpus::find_files(&dir, &|_| true)
                    .iter()
                    .filter_map(|f| f.strip_prefix(&dir).ok())
                    .map(|f| f.to_string_lossy().replace('\\', "/"))
                    .collect()
            });

            if let Some(when) = &rule.when_contains
                && !files.iter().any(|f| glob_match(when, f))
            {
                continue;
            }
            if files.iter().any(|f| glob_match(&rule.require, f)) {
                continue;
            }

            let message = match &rule.when_contains {
                Some(when) => format!(
                    "`{rel}` holds a file matching `{when}` but none matching `{}`.",
                    rule.require
                ),
                None => format!("`{rel}` holds no file matching `{}`.", rule.require),
            };
            out.push(
                Diagnostic::new(rel.clone(), Severity::Error, "missing-file", message)
                    .help(format!("`[[layout]] dir = \"{}\"` requires it.", rule.dir)),
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use crate::{Config, Corpus, lint_corpus};
    use std::fs;
    use std::path::Path;

    fn write(root: &Path, rel: &str) {
        let path = root.join(rel);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, "---\ntitle: T\n---\n\nBody.\n").unwrap();
    }

    fn findings(root: &Path, toml: &str) -> Vec<String> {
        let config = Config::parse(toml).unwrap();
        let corpus = Corpus::load_with(root, config);
        lint_corpus(&corpus)
            .into_iter()
            .map(|d| format!("{}: {}", d.file, d.message))
            .collect()
    }

    const EDITIONS: &str = "[[layout]]\ndir = \"*/editions\"\nrequire = \"*.md\"\n";
    const VARIANTS: &str = "[[layout]]\ndir = \"products/*/_*\"\nwhen_contains = \"*_manual.md\"\nrequire = \"*_nav.json\"\n";

    #[test]
    fn an_empty_collection_is_reported_and_a_full_one_is_not() {
        let tmp = tempdir();
        let root = tmp.path();
        write(root, "documentation/editions/3.10.md");
        fs::create_dir_all(root.join("maintenance/editions")).unwrap();

        let found = findings(root, EDITIONS);
        assert_eq!(found.len(), 1, "{found:?}");
        assert!(found[0].starts_with("maintenance/editions:"), "{found:?}");
    }

    #[test]
    fn the_glob_is_anchored_so_a_nested_editions_is_a_different_thing() {
        let tmp = tempdir();
        let root = tmp.path();
        // `a/b/editions` is not `*/editions`, which is how the TypeScript's
        // "top level only" is said without a depth counter.
        fs::create_dir_all(root.join("documentation/sub/editions")).unwrap();
        write(root, "documentation/editions/3.10.md");
        assert!(findings(root, EDITIONS).is_empty());
    }

    #[test]
    fn a_variant_needs_navigation_only_once_it_has_a_manual() {
        let tmp = tempdir();
        let root = tmp.path();
        write(root, "products/bot600/_base/manual.md");
        write(root, "products/bot600/_lift/thing_manual.md");
        write(root, "products/bot600/_hook/thing_manual.md");
        fs::write(root.join("products/bot600/_hook/thing_nav.json"), "{}").unwrap();

        let found = findings(root, VARIANTS);
        assert_eq!(found.len(), 1, "{found:?}");
        assert!(found[0].starts_with("products/bot600/_lift:"), "{found:?}");
        assert!(found[0].contains("_nav.json"), "{found:?}");
    }

    #[test]
    fn require_says_its_own_depth() {
        let tmp = tempdir();
        let root = tmp.path();
        write(root, "documentation/editions/nested/3.10.md");
        // `*.md` is direct children, so a file one level down does not count…
        assert_eq!(findings(root, EDITIONS).len(), 1);
        // …and `**/*.md` is the subtree.
        let deep = "[[layout]]\ndir = \"*/editions\"\nrequire = \"**/*.md\"\n";
        assert!(findings(root, deep).is_empty());
    }

    #[test]
    fn with_no_layout_section_nothing_is_checked() {
        let tmp = tempdir();
        fs::create_dir_all(tmp.path().join("documentation/editions")).unwrap();
        assert!(findings(tmp.path(), "").is_empty());
    }

    /// A scratch directory that removes itself. Small enough to not be worth
    /// a dependency a language server would then install.
    struct TempDir(std::path::PathBuf);
    impl TempDir {
        fn path(&self) -> &Path {
            &self.0
        }
    }
    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }
    fn tempdir() -> TempDir {
        use std::sync::atomic::{AtomicUsize, Ordering};
        static N: AtomicUsize = AtomicUsize::new(0);
        let path = std::env::temp_dir().join(format!(
            "markdoc-lint-layout-{}-{}",
            std::process::id(),
            N.fetch_add(1, Ordering::Relaxed)
        ));
        let _ = fs::remove_dir_all(&path);
        fs::create_dir_all(&path).unwrap();
        TempDir(path)
    }
}

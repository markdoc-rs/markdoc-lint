//! What a rule needs to know that is not in the file it is linting.
//!
//! Four of the six rules are about whether something *elsewhere* exists: a
//! reference naming an anchor in another page, a `{% partial %}` naming a
//! file, a `{% $variable %}` naming a key in a config. None of those can be
//! answered from one document, so they are answered from an index built
//! once.
//!
//! The index is cheap to build (a parse of each file, which is what the
//! linter was going to do anyway) and expensive to get wrong: an index that
//! is a little bit stale reports a broken reference to a thing that is
//! actually there, which is the fastest way to teach a writer to ignore the
//! linter.

use crate::config::{Config, glob_match};
use markdoc::ext::bundle::Bundle;
use markdoc::ext::{Frontmatter, ParseOptions};
use markdoc::schema::Config as MarkdocConfig;
use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

/// The file at a content root that says how markdoc reads the corpus: the
/// `[parse]` options every tool parses it with, the `[render]` options the
/// renderers add, `[validation]`, and the `[functions]` its pages call. Read
/// after the tag overlay, when there is one.
pub const MARKDOC_FILENAME: &str = "markdoc.toml";

/// Where an anchor was declared.
#[derive(Debug, Clone)]
pub struct Anchor {
    pub file: String,
    pub line: usize,
}

/// Everything the rules need about the corpus as a whole.
#[derive(Debug)]
pub struct Corpus {
    pub root: PathBuf,
    pub config: Config,
    /// Anchor id -> where it was declared, per scope. See
    /// [`Config::scope_of`].
    pub anchors: BTreeMap<String, BTreeMap<String, Anchor>>,
    /// Every partial path that exists, corpus-relative.
    pub partials: BTreeSet<String>,
    /// Variable names a document may use.
    pub variables: BTreeSet<String>,
    /// Values a page should be writing as a variable rather than typing:
    /// each value with the keys that supply it, longest value first so a
    /// name cannot be shadowed by a shorter one it starts with.
    pub literals: Vec<(String, Vec<String>)>,
    /// Files that were read, for reporting.
    pub files: usize,
    /// The schema the rules validate against: markdoc's bundle
    /// (`schema/tags.toml`), this corpus's tag overlay if it declares one,
    /// and its [`MARKDOC_FILENAME`] if it has one, as a markdoc config.
    ///
    /// Built once here rather than per file.
    pub markdoc: MarkdocConfig,
    /// The bundle `markdoc` was made from, for what a config does not hold:
    /// descriptions, aliases, engine labels.
    pub bundle: Bundle,
    /// How every document is parsed: the corpus's `[parse]` options, which
    /// every tool reading it shares so that they agree on what is a tag,
    /// with YAML frontmatter on -- the lint reads frontmatter as data, and
    /// that changes nothing about tags. The renderers' `[render]` options
    /// (the typographer) stay off, so that text positions are exact.
    /// markdoc's own positions (`span`) are off here, as in markdoc: they are
    /// each tool's choice, not the corpus's, and the lint turns them on where
    /// it parses ([`crate::lint_document`] and the anchor index).
    pub parse_options: ParseOptions,
    /// `markdoc` with upstream's built-in tags, nodes and functions merged
    /// in, as `validate` merges them: once, not per document.
    pub(crate) merged: OnceLock<MarkdocConfig>,
}

impl Default for Corpus {
    /// No corpus: markdoc's bundle and upstream's parse options. What an
    /// editor has when a writer opens one loose `.md`.
    fn default() -> Self {
        let bundle = Bundle::builtin();
        Corpus {
            root: PathBuf::new(),
            config: Config::default(),
            anchors: BTreeMap::new(),
            partials: BTreeSet::new(),
            variables: BTreeSet::new(),
            literals: Vec::new(),
            files: 0,
            markdoc: bundle.config(),
            parse_options: lint_options(bundle.parse_options()),
            bundle,
            merged: OnceLock::new(),
        }
    }
}

/// The corpus's parse options as the lint uses them: frontmatter as YAML.
fn lint_options(mut options: ParseOptions) -> ParseOptions {
    options.frontmatter = Frontmatter::Yaml;
    options
}

impl Corpus {
    /// Read a content root, using the `markdoc-lint.toml` found in it.
    pub fn load(root: &Path) -> Self {
        let config = Config::load(root).unwrap_or_default();
        Self::load_with(root, config)
    }

    /// Read a content root with a configuration supplied by the caller —
    /// which is how a CLI reports a malformed config as an error instead of
    /// quietly linting with the defaults.
    pub fn load_with(root: &Path, config: Config) -> Self {
        let mut corpus = Self::schema_only(root, config);
        corpus.load_variables();
        corpus.load_literals();
        corpus.load_markdown();
        corpus
    }

    /// The schema and the parse options of the corpus at `root`, without
    /// its index: no document read, no variable source, `files` 0.
    ///
    /// What a tool that needs to know what a tag *is* in this corpus wants
    /// without linting it (the language server's formatter, run over a
    /// whole corpus): they come from the overlay and [`MARKDOC_FILENAME`],
    /// never from the documents, so building the index for them is wasted.
    pub fn schema_only(root: &Path, config: Config) -> Self {
        let (bundle, markdoc) = Self::markdoc_config(root, &config);
        Corpus {
            root: root.to_path_buf(),
            config,
            markdoc,
            parse_options: lint_options(bundle.parse_options()),
            bundle,
            ..Default::default()
        }
    }

    /// Build the schema for a corpus: markdoc's bundle, plus the overlay
    /// named in `markdoc-lint.toml` if there is one, plus
    /// [`MARKDOC_FILENAME`] at the root if there is one -- in that order, as
    /// every tool reading the corpus applies them, each through
    /// `Bundle::overlay` (`[validation]` and `[functions.*]` included).
    ///
    /// An unreadable or malformed file is reported and then ignored rather
    /// than aborting the lint; the bundle loader applies a file whole or
    /// not at all. The consequence is visible on its own -- every tag the
    /// overlay declares is then reported as undefined -- so the run still
    /// says something true, just more loudly.
    fn markdoc_config(root: &Path, config: &Config) -> (Bundle, MarkdocConfig) {
        let mut bundle = Bundle::builtin();
        if let Some(rel) = &config.schema.overlay {
            let path = root.join(rel);
            match std::fs::read_to_string(&path) {
                Ok(text) => {
                    if let Err(e) = bundle.overlay(&text) {
                        eprintln!("warning: {}: {e}", path.display());
                    }
                }
                Err(e) => eprintln!("warning: cannot read tag overlay {}: {e}", path.display()),
            }
        }
        let path = root.join(MARKDOC_FILENAME);
        if path.is_file() {
            match std::fs::read_to_string(&path) {
                Ok(text) => {
                    if let Err(e) = bundle.overlay(&text) {
                        eprintln!("warning: {}: {e}", path.display());
                    }
                }
                Err(e) => eprintln!("warning: cannot read {}: {e}", path.display()),
            }
        }
        let markdoc = bundle.config();
        (bundle, markdoc)
    }

    /// An empty index with the default configuration, for linting a single
    /// file with no corpus around it — which is what an editor has when a
    /// writer opens one `.md` out of a directory. The rules that need the
    /// index then say nothing rather than reporting everything as missing.
    pub fn empty() -> Self {
        Corpus::default()
    }

    /// [`Corpus::markdoc`] merged with upstream's defaults
    /// (`markdoc::merge_config`), which is what the schema rule
    /// validates against. Merged the first time it is asked for.
    pub fn merged_markdoc(&self) -> &MarkdocConfig {
        self.merged
            .get_or_init(|| markdoc::merge_config(&self.markdoc))
    }

    /// Whether this index knows enough for the cross-document rules to run.
    pub fn is_indexed(&self) -> bool {
        self.files > 0
    }

    fn load_variables(&mut self) {
        let sources = &self.config.variables.sources;
        if sources.is_empty() {
            return;
        }
        for name in &self.config.variables.extra {
            self.variables.insert(name.clone());
        }
        let root = self.root.clone();
        for file in find_files(&root, &|p| {
            let Some(rel) = relative(&root, p) else {
                return false;
            };
            let base = p.file_name().and_then(|s| s.to_str()).unwrap_or("");
            sources
                .iter()
                .any(|pat| glob_match(pat, base) || glob_match(pat, &rel))
        }) {
            let Ok(text) = std::fs::read_to_string(&file) else {
                continue;
            };
            let Ok(json) = serde_json::from_str::<serde_json::Value>(&text) else {
                continue;
            };
            collect_variable_names(&json, String::new(), &mut self.variables);
        }
    }

    /// The corpus-wide values, for `hardcoded-variables`.
    ///
    /// A separate list from `variables`, and deliberately a narrower one:
    /// see that rule for why asking *is this name defined* and *should this
    /// text have been a variable* cannot read the same files.
    fn load_literals(&mut self) {
        let sources = self.config.hardcoded.sources.clone();
        if sources.is_empty() {
            return;
        }
        let root = self.root.clone();
        let mut by_value: BTreeMap<String, Vec<String>> = BTreeMap::new();
        for file in find_files(&root, &|p| {
            let Some(rel) = relative(&root, p) else {
                return false;
            };
            let base = p.file_name().and_then(|s| s.to_str()).unwrap_or("");
            sources
                .iter()
                .any(|pat| glob_match(pat, base) || glob_match(pat, &rel))
        }) {
            let Ok(text) = std::fs::read_to_string(&file) else {
                continue;
            };
            let Ok(serde_json::Value::Object(map)) = serde_json::from_str(&text) else {
                continue;
            };
            for (key, value) in map {
                // `//` is a comment convention in these files, and a nested
                // object is structure rather than something a page types.
                if key.starts_with("//") {
                    continue;
                }
                let serde_json::Value::String(value) = value else {
                    continue;
                };
                if value.is_empty() {
                    continue;
                }
                by_value.entry(value).or_default().push(key);
            }
        }
        self.literals = by_value.into_iter().collect();
        // Longest first: `Bot600` must not match where `Bot600 Shelf Lift`
        // does. Ties broken by value so two runs agree.
        self.literals
            .sort_by(|a, b| b.0.len().cmp(&a.0.len()).then_with(|| a.0.cmp(&b.0)));
    }

    fn load_markdown(&mut self) {
        let root = self.root.clone();
        let mut pages: Vec<(PathBuf, String)> = Vec::new();
        for file in find_files(&root, &|p| {
            p.extension().and_then(|s| s.to_str()) == Some("md")
        }) {
            let Some(rel) = relative(&root, &file) else {
                continue;
            };
            if self.config.is_skipped(&rel) {
                continue;
            }
            self.files += 1;

            if self.config.is_partial(&rel) {
                self.partials.insert(rel.clone());
                // A partial declares no anchors of its own: it is included
                // into pages in several scopes, so an anchor in one would be
                // declared once per inclusion.
                continue;
            }
            pages.push((file, rel));
        }

        let Some(declare) = self.config.anchors.declare_tag.clone() else {
            return;
        };
        let ids = &self.config.anchors.id_attributes;
        let from_frontmatter = self.config.variables.from_frontmatter;
        // An anchor's line is markdoc's own position of its tag, which the
        // parser computes only when asked: the duplicate-anchor finding and
        // the language server's go-to-definition both read it.
        let options = self.parse_options.with_spans(true);
        // Each page on its own, on every core: reading and parsing them is
        // nearly all of what building the index costs.
        let found = in_parallel(&pages, |(file, rel)| {
            let source = std::fs::read_to_string(file).ok()?;
            let tree = markdoc::ext::parse(&source, &options.with_file(rel.as_str())).ok()?;
            let mut anchors: Vec<(String, usize)> = Vec::new();
            let mut fm_keys: Vec<String> = Vec::new();
            crate::walk(&tree, &mut |node| {
                if crate::tag_name(node) == Some(declare.as_str())
                    && let Some(id) = crate::first_str_attr(node, ids)
                {
                    let line = crate::Span::of_node(node).map_or(0, |s| s.start_line);
                    anchors.push((id.trim().to_string(), line));
                }
                // A page's frontmatter may supply variables to the pages
                // that inherit from it.
                if from_frontmatter
                    && matches!(node.node_type, markdoc::types::NodeType::Document)
                    && let Some(markdoc::types::Scalar::Object(fm)) =
                        node.attributes.get("frontmatter")
                {
                    fm_keys.extend(fm.keys().cloned());
                }
            });
            Some((anchors, fm_keys))
        });
        // In path order, whichever thread finished first: the first
        // declaration of an anchor wins.
        for ((_, rel), found) in pages.iter().zip(found) {
            let Some((anchors, fm_keys)) = found else {
                continue;
            };
            let entry = self.anchors.entry(self.config.scope_of(rel)).or_default();
            for (id, line) in anchors {
                entry.entry(id).or_insert(Anchor {
                    file: rel.clone(),
                    line,
                });
            }
            self.variables.extend(fm_keys);
        }
    }

    /// Resolve an anchor as `rel` may see it.
    pub fn anchor(&self, rel: &str, id: &str) -> Option<&Anchor> {
        if self.config.is_partial(rel) {
            // A partial is included from anywhere, so it may name any
            // anchor; it cannot know which page it will land in.
            return self.anchors.values().find_map(|m| m.get(id));
        }
        self.anchors.get(&self.config.scope_of(rel))?.get(id)
    }
}

/// `f` of every item, in the items' order, spread over the machine's cores.
///
/// Scoped threads over contiguous slices rather than a work-stealing pool:
/// the items are documents of similar size, and a dependency for a pool is
/// one more thing a writer installs.
pub fn in_parallel<T: Sync, R: Send>(items: &[T], f: impl Fn(&T) -> R + Sync) -> Vec<R> {
    let threads = std::thread::available_parallelism()
        .map_or(1, |n| n.get())
        .min(items.len().max(1));
    if threads <= 1 {
        return items.iter().map(f).collect();
    }
    let chunk = items.len().div_ceil(threads);
    let f = &f;
    std::thread::scope(|scope| {
        let handles: Vec<_> = items
            .chunks(chunk)
            .map(|slice| scope.spawn(move || slice.iter().map(f).collect::<Vec<R>>()))
            .collect();
        handles
            .into_iter()
            .flat_map(|h| h.join().expect("a worker panicked"))
            .collect()
    })
}

/// Variable names a config file supplies, including nested ones:
/// `{"safety": {"x": 1}}` makes both `safety` and `safety.x` usable.
fn collect_variable_names(value: &serde_json::Value, prefix: String, out: &mut BTreeSet<String>) {
    let serde_json::Value::Object(map) = value else {
        return;
    };
    for (key, child) in map {
        // `//` is a common comment convention in JSON config files.
        if key.starts_with("//") {
            continue;
        }
        let name = if prefix.is_empty() {
            key.clone()
        } else {
            format!("{prefix}.{key}")
        };
        out.insert(name.clone());
        collect_variable_names(child, name, out);
    }
}

fn relative(root: &Path, file: &Path) -> Option<String> {
    Some(
        file.strip_prefix(root)
            .ok()?
            .to_string_lossy()
            .replace('\\', "/"),
    )
}

/// Every directory under `root`, itself included, skipping the ones that
/// are never content.
///
/// The same walk as [`find_files`] and deliberately the same exclusions: a
/// rule about the shape of the corpus must see the same corpus the rules
/// about documents see.
pub fn find_dirs(root: &Path) -> Vec<PathBuf> {
    let mut out = vec![root.to_path_buf()];
    let mut stack = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            let name = path.file_name().and_then(|s| s.to_str()).unwrap_or("");
            if name.starts_with('.') || name == "node_modules" || name == "target" {
                continue;
            }
            if path.is_dir() {
                out.push(path.clone());
                stack.push(path);
            }
        }
    }
    out.sort();
    out
}

/// Every file under `root` matching `want`, skipping the directories that
/// are never content.
pub fn find_files(root: &Path, want: &dyn Fn(&Path) -> bool) -> Vec<PathBuf> {
    let mut out = Vec::new();
    let mut stack = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            let name = path.file_name().and_then(|s| s.to_str()).unwrap_or("");
            if name.starts_with('.') || name == "node_modules" || name == "target" {
                continue;
            }
            if path.is_dir() {
                stack.push(path);
            } else if want(&path) {
                out.push(path);
            }
        }
    }
    out.sort();
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn nested_config_keys_are_usable_at_every_depth() {
        // `{% $safety.pl_d %}` and `{% $safety %}` are both things a page
        // writes, so both have to count as known.
        let json: serde_json::Value =
            serde_json::from_str(r#"{"company":"X","safety":{"pl_d":"PL d"},"//":"ignored"}"#)
                .unwrap();
        let mut out = BTreeSet::new();
        collect_variable_names(&json, String::new(), &mut out);
        assert!(out.contains("company"));
        assert!(out.contains("safety"));
        assert!(out.contains("safety.pl_d"));
        assert!(!out.iter().any(|k| k.starts_with("//")));
    }

    #[test]
    fn an_empty_index_knows_it_is_empty() {
        let c = Corpus::empty();
        assert!(!c.is_indexed());
        assert!(c.anchor("a.md", "anything").is_none());
    }

    /// A content root holding `markdoc-lint.toml` (naming the overlay) and
    /// `files`.
    fn root(name: &str, files: &[(&str, &str)]) -> PathBuf {
        let dir =
            std::env::temp_dir().join(format!("markdoc-lint-corpus-{}-{name}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join(crate::config::CONFIG_FILENAME),
            "[schema]\noverlay = \"tags.overlay.toml\"\n",
        )
        .unwrap();
        for (rel, text) in files {
            std::fs::write(dir.join(rel), text).unwrap();
        }
        dir
    }

    #[test]
    fn markdoc_toml_gives_the_parse_options_validation_and_functions() {
        let dir = root(
            "tables",
            &[
                ("tags.overlay.toml", "[tags.spec]\nself_closing = true\n"),
                (
                    MARKDOC_FILENAME,
                    "[parse]\nallow_indentation = true\n\n[validation]\nvalidate_functions = true\n\n\
                     [functions.higher]\ndescription = \"x\"\n\
                     [functions.higher.parameters.0]\ntype = [\"String\", \"Number\"]\nrequired = true\n",
                ),
            ],
        );
        let corpus = Corpus::schema_only(&dir, Config::load(&dir).unwrap());
        let _ = std::fs::remove_dir_all(&dir);
        assert!(corpus.parse_options.tokenizer.allow_indentation);
        assert!(corpus.markdoc.tags.contains_key("spec"), "the overlay");
        assert!(corpus.markdoc.validation.validate_functions);
        let higher = corpus.markdoc.functions["higher"]
            .as_ref()
            .expect("declared");
        assert!(higher.parameters.as_ref().unwrap()["0"].required);
        assert!(!corpus.is_indexed());
    }

    #[test]
    fn a_file_with_a_fault_changes_nothing() {
        let dir = root(
            "fault",
            &[
                ("tags.overlay.toml", "[tags.spec]\nself_closing = true\n"),
                (
                    MARKDOC_FILENAME,
                    "[tags.other]\nself_closing = true\n\n[validation]\nvalidate_function = true\n",
                ),
            ],
        );
        let corpus = Corpus::schema_only(&dir, Config::load(&dir).unwrap());
        let _ = std::fs::remove_dir_all(&dir);
        assert!(corpus.markdoc.tags.contains_key("spec"), "the overlay");
        assert!(!corpus.markdoc.tags.contains_key("other"));
        assert!(!corpus.markdoc.validation.validate_functions);
    }

    #[test]
    fn the_lint_parses_with_positions_the_corpus_options_leave_off() {
        // markdoc computes its own positions (`span`) only when asked. The
        // corpus's options are every tool's and do not ask; the index and
        // `lint_document` do, because an anchor's line (go-to-definition)
        // and every finding are read from them.
        let page = "# A\n\nText.\n\n## B {% tag \"b_anchor\" /%}\n";
        let dir = root("spans", &[("tags.overlay.toml", ""), ("a.md", page)]);
        let config = Config::parse(
            "[schema]\noverlay = \"tags.overlay.toml\"\n\n[anchors]\ndeclare_tag = \"tag\"\n",
        )
        .unwrap();
        let corpus = Corpus::load_with(&dir, config);
        let _ = std::fs::remove_dir_all(&dir);
        assert!(!corpus.parse_options.args.spans);
        assert_eq!(corpus.anchor("a.md", "b_anchor").map(|a| a.line), Some(5));
        let linted = crate::lint_document("a.md", page, &corpus);
        let heading = &linted.tree.children[2];
        assert_eq!(heading.span.as_ref().map(|s| s.start.line), Some(5));
    }
}

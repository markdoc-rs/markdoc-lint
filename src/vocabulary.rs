//! What a corpus actually writes: every tag, how often and in how many
//! files, each attribute with the values it was given, and whether the
//! schema declares the tag.
//!
//! Read through [`Corpus`], so the report counts the corpus the lint
//! counts: the same files, the same `[parse]` options, and the same "in
//! the schema" set the `schema` rule validates against.
//!
//! A tag is whatever markdoc's tokenizer recognises as one: a tag in a
//! fence is counted, a tag in a code span is text. An annotation, an
//! interpolation and a body that is no tag name no tag. A tag's files are
//! those that open it or write it self-closing.

use crate::Corpus;
use markdoc::IndexMap;
use markdoc::ext::{OccurrenceKind, ParseOptions};
use markdoc::types::Scalar;
use std::collections::{BTreeMap, BTreeSet};
use std::fmt;
use std::path::PathBuf;

/// How many distinct string values an attribute's record keeps. One with
/// more is not a fixed set, and the report says so (`capped`) rather than
/// holding every free-text value the corpus contains.
pub const VALUE_CAP: usize = 64;

/// The most distinct values an attribute may have been given and still be
/// reported as a closed set — a `one_of` candidate.
pub const CLOSED_SET_MAX: usize = 12;

/// How the corpus uses one tag.
#[derive(Debug, Clone, Default)]
pub struct TagUse {
    /// `{% name %}`, opening a block.
    pub open: usize,
    /// `{% name /%}`.
    pub self_closing: usize,
    /// `{% /name %}`.
    pub close: usize,
    /// Uses that give a primary value: `{% name "value" %}`.
    pub primary: usize,
    /// Every attribute but the primary value, by name.
    pub attributes: BTreeMap<String, AttributeUse>,
    /// Corpus-relative paths of the files that open the tag or write it
    /// self-closing.
    pub files: BTreeSet<String>,
}

impl TagUse {
    /// Every use: opening and self-closing. A closing tag is part of the
    /// use it closes.
    pub fn uses(&self) -> usize {
        self.open + self.self_closing
    }

    /// `block`, `self-closing`, `both`, or `close-only` for a tag that is
    /// only ever closed.
    pub fn form(&self) -> &'static str {
        match (self.open > 0, self.self_closing > 0) {
            (true, true) => "both",
            (true, false) => "block",
            (false, true) => "self-closing",
            (false, false) => "close-only",
        }
    }

    fn record(&mut self, attributes: &IndexMap<String, Scalar>, rel: &str) {
        if !self.files.contains(rel) {
            self.files.insert(rel.to_string());
        }
        for (name, value) in attributes {
            if name == "primary" {
                self.primary += 1;
                continue;
            }
            self.attributes
                .entry(name.clone())
                .or_default()
                .record(value);
        }
    }
}

/// How the corpus uses one attribute of one tag.
#[derive(Debug, Clone, Default)]
pub struct AttributeUse {
    /// Times it was written.
    pub uses: usize,
    /// The string literals it was given, each with how often: at most
    /// [`VALUE_CAP`] distinct ones, the first met in path order.
    pub values: BTreeMap<String, usize>,
    /// It was given more than [`VALUE_CAP`] distinct strings.
    pub capped: bool,
    /// It was given a literal that is not a string: a number, a boolean,
    /// `null`, an array or an object.
    pub non_string: bool,
    /// It was given a variable or a function call.
    pub expression: bool,
}

impl AttributeUse {
    /// Whether every value it was given is one of a few strings — at most
    /// [`CLOSED_SET_MAX`], none of them an expression or another type — so
    /// that the schema could say so with `one_of`.
    pub fn is_closed_set(&self) -> bool {
        !self.non_string
            && !self.expression
            && !self.capped
            && (1..=CLOSED_SET_MAX).contains(&self.values.len())
    }

    fn record(&mut self, value: &Scalar) {
        self.uses += 1;
        match value {
            Scalar::String(s) => {
                if let Some(n) = self.values.get_mut(s) {
                    *n += 1;
                } else if self.values.len() < VALUE_CAP {
                    self.values.insert(s.clone(), 1);
                } else {
                    self.capped = true;
                }
            }
            Scalar::Variable(_) | Scalar::Function(_) => self.expression = true,
            _ => self.non_string = true,
        }
    }

    /// Its values, most used first, at most [`CLOSED_SET_MAX`] of them.
    fn commonest(&self) -> Vec<(&String, usize)> {
        let mut values: Vec<(&String, usize)> = self.values.iter().map(|(v, n)| (v, *n)).collect();
        // Stable, so values used equally often stay in name order.
        values.sort_by_key(|&(_, n)| std::cmp::Reverse(n));
        values.truncate(CLOSED_SET_MAX);
        values
    }
}

/// The tag vocabulary of a corpus. See the [module](self).
///
/// `Display` prints the report; [`Vocabulary::draft_overlay`] the schema the
/// corpus would need for what it writes, and [`Vocabulary::to_json`] the
/// report as one JSON object.
#[derive(Debug, Clone, Default)]
pub struct Vocabulary {
    /// The tag overlay the schema was built with, as configured: `[schema]
    /// overlay` of `markdoc-lint.toml`, relative to the root, or the absolute
    /// path the command line's `--overlay` put in its place.
    pub overlay: Option<String>,
    /// Documents read.
    pub files: usize,
    /// Every tag written, by name.
    pub tags: BTreeMap<String, TagUse>,
    /// Every tag the schema declares. See the [module](self).
    pub declared: BTreeSet<String>,
    /// Documents the tokenizer refused, with what it said, which are not
    /// in `files`.
    pub unparsable: Vec<(String, String)>,
}

impl Vocabulary {
    /// Every document the lint reads under the corpus root.
    pub fn of_corpus(corpus: &Corpus) -> Self {
        let files = crate::corpus::find_files(&corpus.root, &|p| {
            p.extension().and_then(|s| s.to_str()) == Some("md")
        });
        Self::of_files(corpus, &files)
    }

    /// The documents `files` names, as the lint reads them: one outside the
    /// corpus root, one `[corpus] skip` excludes and one that cannot be read
    /// are left out. Read on every core, counted in the order given.
    pub fn of_files(corpus: &Corpus, files: &[PathBuf]) -> Self {
        let mut vocabulary = Vocabulary {
            overlay: corpus.config.schema.overlay.clone(),
            declared: crate::rules::schema::declared(corpus).into_iter().collect(),
            ..Default::default()
        };
        let options = &corpus.parse_options;
        let found = crate::corpus::in_parallel(files, |file| {
            let rel = crate::corpus::relative(&corpus.root, file)?;
            if corpus.config.is_skipped(&rel) {
                return None;
            }
            let source = std::fs::read_to_string(file).ok()?;
            let occurrences = markdoc::ext::occurrences(&source, options)
                .map(|found| found.into_iter().map(|o| o.kind).collect::<Vec<_>>())
                .map_err(|e| markdoc::thrown(&e).to_string());
            Some((rel, occurrences))
        });
        for (rel, occurrences) in found.into_iter().flatten() {
            match occurrences {
                Ok(kinds) => vocabulary.add(&rel, kinds),
                Err(message) => vocabulary.unparsable.push((rel, message)),
            }
        }
        vocabulary
    }

    /// Count one document's text, tokenized with `options`.
    pub fn add_source(
        &mut self,
        rel: &str,
        source: &str,
        options: &ParseOptions,
    ) -> markdoc::types::Result<()> {
        let found = markdoc::ext::occurrences(source, options)?;
        self.add(rel, found.into_iter().map(|o| o.kind).collect());
        Ok(())
    }

    fn add(&mut self, rel: &str, kinds: Vec<OccurrenceKind>) {
        self.files += 1;
        for kind in kinds {
            match kind {
                OccurrenceKind::Open { name, attributes } => {
                    let tag = self.tags.entry(name).or_default();
                    tag.open += 1;
                    tag.record(&attributes, rel);
                }
                OccurrenceKind::SelfClosing { name, attributes } => {
                    let tag = self.tags.entry(name).or_default();
                    tag.self_closing += 1;
                    tag.record(&attributes, rel);
                }
                OccurrenceKind::Close { name } => self.tags.entry(name).or_default().close += 1,
                OccurrenceKind::Annotation { .. }
                | OccurrenceKind::Interpolation { .. }
                | OccurrenceKind::Error { .. } => {}
            }
        }
    }

    /// Whether the schema declares `tag`.
    pub fn is_declared(&self, tag: &str) -> bool {
        self.declared.contains(tag)
    }

    /// What the schema declares and the corpus never writes.
    pub fn unused(&self) -> impl Iterator<Item = &str> {
        self.declared
            .iter()
            .filter(|name| !self.tags.contains_key(*name))
            .map(String::as_str)
    }

    /// A draft overlay, in markdoc's tag-schema format, for every tag the
    /// corpus writes and the schema does not declare.
    ///
    /// A starting point, not a schema: every attribute is an optional
    /// string, and one that is a closed set gets its values as `one_of`. The
    /// primary value is declared like any attribute, because nothing
    /// declares it implicitly. No `inline` is written: left out, a tag may
    /// be block or inline, while a declared value is enforced, and the
    /// tokenizer cannot tell where a tag was placed.
    pub fn draft_overlay(&self) -> String {
        let mut out = String::new();
        for (name, tag) in &self.tags {
            if self.is_declared(name) {
                continue;
            }
            out.push_str(&format!("[tags.{name}]\n"));
            out.push_str("description = \"TODO\"\n");
            out.push_str(&format!("self_closing = {}\n", tag.open == 0));
            let primary = AttributeUse::default();
            let primary = (tag.primary > 0).then_some(("primary", &primary));
            let attributes = tag.attributes.iter().map(|(a, u)| (a.as_str(), u));
            for (attribute, used) in primary.into_iter().chain(attributes) {
                out.push_str(&format!("\n[tags.{name}.attributes.{attribute}]\n"));
                out.push_str("type = [\"String\"]\n");
                out.push_str("required = false\n");
                if used.is_closed_set() {
                    let values: Vec<String> = used
                        .values
                        .keys()
                        .map(|v| toml::Value::String(v.clone()).to_string())
                        .collect();
                    out.push_str(&format!("one_of = [{}]\n", values.join(", ")));
                }
            }
            out.push('\n');
        }
        out
    }

    /// The report as one JSON object: `files`, `overlay`, `tags` (by name:
    /// `uses`, `open`, `self_closing`, `close`, `primary`, `files` as a
    /// count, `form`, `in_schema` and `attributes`), `unused` and
    /// `unparsable`. An attribute has `uses`, `values` (each with its count),
    /// `capped`, `non_string`, `expression` and `closed_set`.
    pub fn to_json(&self) -> serde_json::Value {
        let tags: serde_json::Map<String, serde_json::Value> = self
            .tags
            .iter()
            .map(|(name, tag)| {
                let attributes: serde_json::Map<String, serde_json::Value> = tag
                    .attributes
                    .iter()
                    .map(|(a, u)| {
                        let value = serde_json::json!({
                            "uses": u.uses,
                            "values": u.values,
                            "capped": u.capped,
                            "non_string": u.non_string,
                            "expression": u.expression,
                            "closed_set": u.is_closed_set(),
                        });
                        (a.clone(), value)
                    })
                    .collect();
                let value = serde_json::json!({
                    "uses": tag.uses(),
                    "open": tag.open,
                    "self_closing": tag.self_closing,
                    "close": tag.close,
                    "primary": tag.primary,
                    "files": tag.files.len(),
                    "form": tag.form(),
                    "in_schema": self.is_declared(name),
                    "attributes": attributes,
                });
                (name.clone(), value)
            })
            .collect();
        let unparsable: Vec<serde_json::Value> = self
            .unparsable
            .iter()
            .map(|(file, message)| serde_json::json!({ "file": file, "message": message }))
            .collect();
        serde_json::json!({
            "files": self.files,
            "overlay": self.overlay,
            "declared": self.declared.len(),
            "tags": tags,
            "unused": self.unused().collect::<Vec<_>>(),
            "unparsable": unparsable,
        })
    }
}

impl fmt::Display for Vocabulary {
    /// The report: the totals, one line per tag, each tag's attributes with
    /// their commonest values (`CLOSED` marks a closed set), and what the
    /// schema declares that the corpus never writes.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        writeln!(
            f,
            "overlay           {}",
            self.overlay.as_deref().unwrap_or("none")
        )?;
        writeln!(f, "files parsed      {}", self.files)?;
        writeln!(f, "distinct tags     {}", self.tags.len())?;
        writeln!(f, "declared tags     {}\n", self.declared.len())?;
        writeln!(
            f,
            "{:<20} {:>7} {:>7} {:>6}  {:<9} form",
            "tag", "uses", "files", "attrs", "in schema"
        )?;
        for (name, tag) in &self.tags {
            writeln!(
                f,
                "{:<20} {:>7} {:>7} {:>6}  {:<9} {}",
                name,
                tag.uses(),
                tag.files.len(),
                tag.attributes.len(),
                if self.is_declared(name) { "yes" } else { "NO" },
                tag.form()
            )?;
        }
        writeln!(f, "\n── attributes, with observed value sets ──")?;
        for (name, tag) in &self.tags {
            if tag.attributes.is_empty() && tag.primary == 0 {
                continue;
            }
            writeln!(f, "\n{name}")?;
            if tag.primary > 0 {
                writeln!(f, "    (primary)        {:>6}", tag.primary)?;
            }
            for (attribute, used) in &tag.attributes {
                let values = if !used.values.is_empty() {
                    let shown: Vec<String> = used
                        .commonest()
                        .iter()
                        .map(|(v, n)| format!("{v}({n})"))
                        .collect();
                    shown.join(" ")
                } else if used.expression {
                    "<expressions>".to_string()
                } else {
                    "<non-string>".to_string()
                };
                writeln!(
                    f,
                    "    {:<16} {:>6}  {}{}",
                    attribute,
                    used.uses,
                    if used.is_closed_set() { "CLOSED " } else { "" },
                    values
                )?;
            }
        }
        let unused: Vec<&str> = self.unused().collect();
        writeln!(
            f,
            "\n── in schema, never used in this corpus ({}) ──",
            unused.len()
        )?;
        writeln!(f, "{}", unused.join(" "))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Config;
    use std::path::Path;

    fn vocabulary_of(text: &str, options: &ParseOptions) -> Vocabulary {
        let mut vocabulary = Vocabulary::default();
        vocabulary.add_source("f.md", text, options).unwrap();
        vocabulary
    }

    /// A small product manual: two pages, a draft `[corpus] skip` excludes,
    /// a hidden directory the walk never enters, and a tag overlay that
    /// declares `spec`.
    fn manual(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "markdoc-lint-vocabulary-{}-{name}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        for sub in ["guide", "drafts", ".cache"] {
            std::fs::create_dir_all(dir.join(sub)).unwrap();
        }
        let files: &[(&str, &str)] = &[
            (
                "markdoc-lint.toml",
                "[schema]\noverlay = \"tags.overlay.toml\"\n\n\
                 [corpus]\nskip = [\"drafts/**\", \".*\", \".*/**\"]\n",
            ),
            (
                "tags.overlay.toml",
                "[tags.spec]\nself_closing = true\n\n\
                 [tags.spec.attributes.name]\ntype = [\"String\"]\n",
            ),
            (
                "badge.overlay.toml",
                "[tags.badge]\n\n[tags.badge.attributes.color]\ntype = [\"String\"]\n",
            ),
            (
                "guide/install.md",
                "# Install\n\n\
                 {% callout type=\"note\" %}\nRead this first.\n{% /callout %}\n\n\
                 {% spec name=\"Weight\" /%}\n\n\
                 {% badge color=\"green\" %}New{% /badge %}\n\n\
                 {% if $beta %}\nBeta only.\n{% /if %}\n",
            ),
            (
                "guide/use.md",
                "# Use\n\n\
                 {% callout type=\"warning\" %}\nMind the gap.\n{% /callout %}\n\n\
                 {% spec name=\"Height\" /%}\n\n\
                 {% badge color=\"red\" size=2 /%}\n\n\
                 {% video \"intro\" src=$clip /%}\n\n\
                 Inline `{% callout %}` is text.\n",
            ),
            ("drafts/wip.md", "{% secret /%}\n"),
            (".cache/old.md", "{% stale /%}\n"),
        ];
        for (rel, text) in files {
            std::fs::write(dir.join(rel), text).unwrap();
        }
        dir
    }

    fn report(dir: &Path, config: Config) -> Vocabulary {
        Vocabulary::of_corpus(&Corpus::schema_only(dir, config))
    }

    #[test]
    fn it_counts_what_the_lint_reads_and_nothing_it_skips() {
        let dir = manual("counts");
        let v = report(&dir, Config::load(&dir).unwrap());
        let _ = std::fs::remove_dir_all(&dir);
        assert_eq!(v.files, 2);
        // `secret` is in a skipped directory, `stale` in a hidden one, and
        // the callout in a code span is text.
        assert_eq!(
            v.tags.keys().collect::<Vec<_>>(),
            ["badge", "callout", "if", "spec", "video"]
        );
        let callout = &v.tags["callout"];
        assert_eq!(
            (callout.uses(), callout.open, callout.close),
            (2, 2, 2),
            "{callout:?}"
        );
        assert_eq!(callout.files.len(), 2);
        assert_eq!(callout.form(), "block");
        assert_eq!(v.tags["badge"].form(), "both");
        assert_eq!(v.tags["spec"].form(), "self-closing");
        let video = &v.tags["video"];
        assert_eq!(video.primary, 1);
        assert!(video.attributes["src"].expression);
        assert!(v.tags["badge"].attributes["size"].non_string);
        assert!(v.unparsable.is_empty());
    }

    #[test]
    fn in_schema_is_the_bundle_the_overlay_and_upstreams_own_tags() {
        let dir = manual("schema");
        let v = report(&dir, Config::load(&dir).unwrap());
        // The overlay replaced: what `--overlay` does.
        let mut config = Config::load(&dir).unwrap();
        config.schema.overlay = Some("badge.overlay.toml".to_string());
        let other = report(&dir, config);
        let _ = std::fs::remove_dir_all(&dir);

        assert!(v.is_declared("callout"), "markdoc's bundle");
        assert!(v.is_declared("spec"), "the overlay");
        assert!(v.is_declared("if"), "upstream's built-ins");
        assert!(!v.is_declared("badge"));
        assert!(!v.is_declared("video"));
        assert!(v.unused().any(|t| t == "slot"));
        assert!(!v.unused().any(|t| t == "callout"));

        assert_eq!(other.overlay.as_deref(), Some("badge.overlay.toml"));
        assert!(other.is_declared("badge"));
        assert!(!other.is_declared("spec"));
    }

    #[test]
    fn a_handful_of_strings_is_a_closed_set_and_anything_else_is_not() {
        let dir = manual("closed");
        let v = report(&dir, Config::load(&dir).unwrap());
        let _ = std::fs::remove_dir_all(&dir);
        let kind = &v.tags["callout"].attributes["type"];
        assert!(kind.is_closed_set());
        assert_eq!(kind.values.keys().collect::<Vec<_>>(), ["note", "warning"]);
        assert!(
            !v.tags["video"].attributes["src"].is_closed_set(),
            "a variable"
        );
        assert!(
            !v.tags["badge"].attributes["size"].is_closed_set(),
            "a number"
        );

        // Past the cap the values stop being recorded, the ones already
        // recorded are still counted, and it is no closed set.
        let mut text = String::new();
        for i in 0..=VALUE_CAP {
            text.push_str(&format!("{{% chip label=\"v{i}\" /%}}\n\n"));
        }
        text.push_str("{% chip label=\"v0\" /%}\n");
        let label =
            &vocabulary_of(&text, &ParseOptions::default()).tags["chip"].attributes["label"];
        assert_eq!(label.uses, VALUE_CAP + 2);
        assert_eq!(label.values.len(), VALUE_CAP);
        assert_eq!(label.values["v0"], 2);
        assert!(label.capped && !label.is_closed_set());
    }

    #[test]
    fn it_reads_with_the_corpus_parse_options() {
        // Inside an HTML comment a tag is comment text, but only with
        // `allow_comments`, which a corpus sets in its `markdoc.toml`.
        let text = "Before <!-- {% callout /%} --> after.\n";
        let default = vocabulary_of(text, &ParseOptions::default());
        assert_eq!(default.tags["callout"].self_closing, 1);

        let dir = manual("options");
        std::fs::write(
            dir.join(crate::corpus::MARKDOC_FILENAME),
            "[parse]\nallow_comments = true\n",
        )
        .unwrap();
        std::fs::write(dir.join("guide/comment.md"), text).unwrap();
        let v = report(&dir, Config::load(&dir).unwrap());
        let _ = std::fs::remove_dir_all(&dir);
        assert_eq!(v.files, 3);
        assert_eq!(v.tags["callout"].self_closing, 0);
    }

    #[test]
    fn the_draft_overlay_declares_what_the_schema_lacks() {
        let dir = manual("draft");
        let v = report(&dir, Config::load(&dir).unwrap());
        let _ = std::fs::remove_dir_all(&dir);
        let draft = v.draft_overlay();
        assert_eq!(
            draft,
            "[tags.badge]\n\
             description = \"TODO\"\n\
             self_closing = false\n\
             \n[tags.badge.attributes.color]\n\
             type = [\"String\"]\n\
             required = false\n\
             one_of = [\"green\", \"red\"]\n\
             \n[tags.badge.attributes.size]\n\
             type = [\"String\"]\n\
             required = false\n\
             \n\
             [tags.video]\n\
             description = \"TODO\"\n\
             self_closing = true\n\
             \n[tags.video.attributes.primary]\n\
             type = [\"String\"]\n\
             required = false\n\
             \n[tags.video.attributes.src]\n\
             type = [\"String\"]\n\
             required = false\n\
             \n"
        );
        // And markdoc reads it as an overlay.
        let mut bundle = markdoc::ext::bundle::Bundle::builtin();
        bundle.overlay(&draft).unwrap();
        assert!(bundle.tag_names().any(|t| t == "video"));
    }

    #[test]
    fn a_value_with_a_quote_is_quoted_as_toml() {
        let v = vocabulary_of(
            "{% chip label=\"say \\\"hi\\\"\" /%}\n",
            &ParseOptions::default(),
        );
        let draft = v.draft_overlay();
        let table: toml::Table = toml::from_str(&draft).unwrap();
        let one_of = &table["tags"]["chip"]["attributes"]["label"]["one_of"];
        assert_eq!(one_of[0].as_str(), Some("say \"hi\""));
    }

    #[test]
    fn the_json_is_the_report() {
        let dir = manual("json");
        let v = report(&dir, Config::load(&dir).unwrap());
        let _ = std::fs::remove_dir_all(&dir);
        let json = v.to_json();
        assert_eq!(json["files"], 2);
        assert_eq!(json["overlay"], "tags.overlay.toml");
        let callout = &json["tags"]["callout"];
        assert_eq!(callout["uses"], 2);
        assert_eq!(callout["files"], 2);
        assert_eq!(callout["form"], "block");
        assert_eq!(callout["in_schema"], true);
        assert_eq!(callout["attributes"]["type"]["values"]["note"], 1);
        assert_eq!(callout["attributes"]["type"]["closed_set"], true);
        assert_eq!(json["tags"]["badge"]["in_schema"], false);
        assert_eq!(
            json["tags"]["video"]["attributes"]["src"]["expression"],
            true
        );
        assert!(
            json["unused"]
                .as_array()
                .unwrap()
                .contains(&serde_json::json!("slot"))
        );
    }

    #[test]
    fn the_text_report_has_a_line_per_tag_and_marks_closed_sets() {
        let dir = manual("text");
        let v = report(&dir, Config::load(&dir).unwrap());
        let _ = std::fs::remove_dir_all(&dir);
        let text = v.to_string();
        assert!(
            text.starts_with("overlay           tags.overlay.toml\n"),
            "{text}"
        );
        assert!(text.contains("files parsed      2\n"), "{text}");
        assert!(text.contains("distinct tags     5\n"), "{text}");
        assert!(
            text.contains("badge                      2       2      2  NO        both\n"),
            "{text}"
        );
        assert!(
            text.contains("    type                  2  CLOSED note(1) warning(1)\n"),
            "{text}"
        );
        assert!(
            text.contains("    src                   1  <expressions>\n"),
            "{text}"
        );
    }
}

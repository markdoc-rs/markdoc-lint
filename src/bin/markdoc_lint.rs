//! `markdoc-lint` — the command line face of the rules.
//!
//! Usage:
//!     markdoc-lint [--json] [--root <dir>] [path…]
//!     markdoc-lint --vocabulary [--json | --schema] [--overlay <file>] [--root <dir>] [path…]
//!
//! With no paths, every `.md` under the root. With paths, only those — but
//! the index is still built from the whole root, because a rule about
//! whether an anchor exists somewhere else cannot be answered from the file
//! being linted.
//!
//! Exit code is 1 if anything was reported at `error`, 0 otherwise. A
//! warning does not fail a build: a writer who cannot merge until every
//! style hint is gone stops reading them.
//!
//! `--vocabulary` reports which tags, attributes and values the documents
//! the lint would read contain, instead of what is wrong with them
//! (`markdoc_lint::vocabulary`), and exits 0 whatever it finds.

use markdoc_lint::config::{CONFIG_FILENAME, Config};
use markdoc_lint::{Corpus, Severity, Vocabulary, corpus, lint_document};
use std::path::{Path, PathBuf};

fn main() -> std::process::ExitCode {
    let mut root = PathBuf::from(".");
    let mut json = false;
    let mut vocabulary = false;
    let mut draft = false;
    let mut overlay: Option<PathBuf> = None;
    let mut paths: Vec<PathBuf> = Vec::new();
    let mut args = std::env::args().skip(1);

    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--json" => json = true,
            "--root" => match args.next() {
                Some(v) => root = PathBuf::from(v),
                None => return fail("--root needs a directory"),
            },
            "--vocabulary" => vocabulary = true,
            "--schema" => draft = true,
            "--overlay" => match args.next() {
                Some(v) => overlay = Some(PathBuf::from(v)),
                None => return fail("--overlay needs a file"),
            },
            "-h" | "--help" => {
                println!("{HELP}");
                return std::process::ExitCode::SUCCESS;
            }
            "--version" => {
                println!("markdoc-lint {}", env!("CARGO_PKG_VERSION"));
                return std::process::ExitCode::SUCCESS;
            }
            other if other.starts_with('-') => {
                return fail(&format!("unknown option {other}"));
            }
            other => paths.push(PathBuf::from(other)),
        }
    }

    if !vocabulary && (draft || overlay.is_some()) {
        return fail("--schema and --overlay are options of --vocabulary");
    }
    if draft && json {
        return fail("--schema prints a tag overlay, which is TOML, not JSON");
    }

    let root = match root.canonicalize() {
        Ok(r) => r,
        Err(e) => return fail(&format!("{}: {e}", root.display())),
    };

    // A malformed config is fatal rather than ignored. Falling back to the
    // defaults would turn a typo into "the linter stopped finding things",
    // which is the failure hardest to notice.
    let mut config = match Config::load(&root) {
        Ok(c) => c,
        Err(e) => return fail(&e.to_string()),
    };
    // Relative to where the command was run, as every other path on the
    // command line is; `[schema] overlay` is relative to the root, and an
    // absolute path is one the root does not change.
    if let Some(path) = overlay {
        if !path.is_file() {
            return fail(&format!("{}: no such file", path.display()));
        }
        match std::path::absolute(&path).map(|p| p.into_os_string().into_string()) {
            Ok(Ok(p)) => config.schema.overlay = Some(p),
            Ok(Err(_)) => return fail(&format!("{}: not a UTF-8 path", path.display())),
            Err(e) => return fail(&format!("{}: {e}", path.display())),
        }
    }
    let configured = root.join(CONFIG_FILENAME).is_file();

    let targets: Vec<PathBuf> = if paths.is_empty() {
        corpus::find_files(&root, &|p| {
            p.extension().and_then(|s| s.to_str()) == Some("md")
        })
    } else {
        paths
            .iter()
            .filter_map(|p| p.canonicalize().ok())
            .flat_map(|p| {
                if p.is_dir() {
                    corpus::find_files(&p, &|q| {
                        q.extension().and_then(|s| s.to_str()) == Some("md")
                    })
                } else {
                    vec![p]
                }
            })
            .collect()
    };

    if vocabulary {
        // The schema and the parse options, without the index: the report
        // is about what each document writes, not about what it refers to.
        let corpus = Corpus::schema_only(&root, config);
        return report_vocabulary(&corpus, &targets, json, draft, configured);
    }
    let index = Corpus::load_with(&root, config);

    // The shape of the corpus, once, and only when the whole of it was
    // asked about: a finding that `editions/` is empty is not about a file
    // somebody named on the command line.
    let mut all = if paths.is_empty() {
        markdoc_lint::lint_corpus(&index)
    } else {
        Vec::new()
    };
    // Every document on its own, on every core, and the findings in the
    // order the files were named or found, so two runs print the same.
    let found = corpus::in_parallel(&targets, |file| {
        let rel = rel_of(&root, file)?;
        if index.config.is_skipped(&rel) {
            return None;
        }
        let source = std::fs::read_to_string(file).ok()?;
        Some(lint_document(&rel, &source, &index).diagnostics)
    });
    let mut linted = 0usize;
    for diagnostics in found.into_iter().flatten() {
        linted += 1;
        all.extend(diagnostics);
    }

    let errors = all.iter().filter(|d| d.severity == Severity::Error).count();
    let warnings = all
        .iter()
        .filter(|d| d.severity == Severity::Warning)
        .count();

    if json {
        // One object, not a stream: a caller that wants to count has to read
        // the whole thing anyway, and a half-written stream after a crash
        // looks exactly like a clean run.
        let out = serde_json::json!({
            "files": linted,
            "indexed": index.files,
            "configured": configured,
            "errors": errors,
            "warnings": warnings,
            "diagnostics": all,
        });
        println!("{}", serde_json::to_string_pretty(&out).unwrap_or_default());
    } else {
        for d in &all {
            println!("{}", d.to_text());
        }
        eprintln!(
            "\n{linted} file(s) linted against an index of {}; {errors} error(s), \
             {warnings} warning(s).",
            index.files
        );
        if !configured {
            // Otherwise a corpus with a misplaced config file looks clean.
            eprintln!(
                "note: no {CONFIG_FILENAME} in {}, so only the Markdoc-level rules ran.",
                root.display()
            );
        }
    }

    if errors > 0 {
        std::process::ExitCode::FAILURE
    } else {
        std::process::ExitCode::SUCCESS
    }
}

/// `--vocabulary`: the report, the draft overlay (`--schema`) or the JSON.
fn report_vocabulary(
    corpus: &Corpus,
    targets: &[PathBuf],
    json: bool,
    draft: bool,
    configured: bool,
) -> std::process::ExitCode {
    let vocabulary = Vocabulary::of_files(corpus, targets);
    if json {
        let mut out = vocabulary.to_json();
        out["configured"] = serde_json::Value::Bool(configured);
        println!("{}", serde_json::to_string_pretty(&out).unwrap_or_default());
        return std::process::ExitCode::SUCCESS;
    }
    if draft {
        print!("{}", vocabulary.draft_overlay());
    } else {
        print!("{vocabulary}");
    }
    for (file, message) in &vocabulary.unparsable {
        eprintln!("warning: {file}: {message}");
    }
    if !configured {
        // `--overlay` reads an overlay without a config file.
        let overlay = match corpus.config.schema.overlay {
            Some(_) => "",
            None => "no tag overlay was read and ",
        };
        eprintln!(
            "note: no {CONFIG_FILENAME} in {}, so {overlay}only the default \
             [corpus] skip applied.",
            corpus.root.display()
        );
    }
    std::process::ExitCode::SUCCESS
}

fn rel_of(root: &Path, file: &Path) -> Option<String> {
    Some(
        file.strip_prefix(root)
            .ok()?
            .to_string_lossy()
            .replace('\\', "/"),
    )
}

fn fail(msg: &str) -> std::process::ExitCode {
    eprintln!("markdoc-lint: {msg}\n\n{HELP}");
    std::process::ExitCode::FAILURE
}

const HELP: &str = "\
markdoc-lint [--json] [--root <dir>] [path…]
markdoc-lint --vocabulary [--json | --schema] [--overlay <file>] [--root <dir>] [path…]

  --root <dir>   the content root. Defaults to the working directory. The
                 index of anchors, partials and variables is always built
                 from the whole of it, even when linting one file: a broken
                 cross-reference cannot be seen from one document.
  --json         one JSON object on stdout, for CI.
  --vocabulary   report what the corpus writes instead of linting it: every
                 tag, how often and in how many files, its attributes with
                 the values they were given, and whether the schema declares
                 it. The same files, read with the same options, as a lint.
  --schema       with --vocabulary: print a draft tag overlay for the tags
                 the schema does not declare, instead of the report.
  --overlay <file>
                 with --vocabulary: read this tag overlay instead of the one
                 markdoc-lint.toml names.
  --version      print the version.

Conventions come from markdoc-lint.toml at the root. With no such file only
the rules that are about Markdoc itself run.

Exits 1 if anything was reported at error level.";

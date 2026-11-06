//! `markdoc-lint` — the command line face of the rules.
//!
//! Usage:
//!     markdoc-lint [--json] [--root <dir>] [path…]
//!
//! With no paths, every `.md` under the root. With paths, only those — but
//! the index is still built from the whole root, because a rule about
//! whether an anchor exists somewhere else cannot be answered from the file
//! being linted.
//!
//! Exit code is 1 if anything was reported at `error`, 0 otherwise. A
//! warning does not fail a build: a writer who cannot merge until every
//! style hint is gone stops reading them.

use markdoc_lint::config::{CONFIG_FILENAME, Config};
use markdoc_lint::{Corpus, Severity, corpus, lint_document};
use std::path::{Path, PathBuf};

fn main() -> std::process::ExitCode {
    let mut root = PathBuf::from(".");
    let mut json = false;
    let mut paths: Vec<PathBuf> = Vec::new();
    let mut args = std::env::args().skip(1);

    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--json" => json = true,
            "--root" => match args.next() {
                Some(v) => root = PathBuf::from(v),
                None => return fail("--root needs a directory"),
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

    let root = match root.canonicalize() {
        Ok(r) => r,
        Err(e) => return fail(&format!("{}: {e}", root.display())),
    };

    // A malformed config is fatal rather than ignored. Falling back to the
    // defaults would turn a typo into "the linter stopped finding things",
    // which is the failure hardest to notice.
    let config = match Config::load(&root) {
        Ok(c) => c,
        Err(e) => return fail(&e.to_string()),
    };
    let configured = root.join(CONFIG_FILENAME).is_file();
    let index = Corpus::load_with(&root, config);

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

  --root <dir>   the content root. Defaults to the working directory. The
                 index of anchors, partials and variables is always built
                 from the whole of it, even when linting one file: a broken
                 cross-reference cannot be seen from one document.
  --json         one JSON object on stdout, for CI.
  --version      print the version.

Conventions come from markdoc-lint.toml at the root. With no such file only
the rules that are about Markdoc itself run.

Exits 1 if anything was reported at error level.";

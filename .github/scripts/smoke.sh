#!/usr/bin/env bash
#
# The markdoc-lint binary, against real documents.
#
# Every test in the crate hands `lint_document` a string. This is the one
# check that runs the program a CI job runs: the config read from disk, the
# walk that finds the files, the index, the corpus rules, the JSON envelope,
# the text form and the exit code. A rule that stops firing fails it.
#
# The corpora are built here rather than kept as a fixture directory. The
# smoke test this replaces ran `markdoc-lint --root examples`, and examples/
# holds a markdoc-lint.toml and no documents -- so it indexed zero files and
# would have passed with every rule removed. Each run below asserts how many
# files it read before it asserts anything about what they contained.
#
# The second corpus is linted under examples/markdoc-lint.toml as it stands,
# because that file is what the README points a reader at: if it stops
# parsing, or a section of it stops turning its rule on, this says so.
#
# Usage:
#   smoke.sh <path to a markdoc-lint binary>
#
# check.sh runs it on the release build, ci.yml on a debug build on Windows
# and macOS, and release.yml on every binary it is about to publish but
# one: macos-x86_64 is cross-built on an Apple Silicon runner, and the step
# that runs this skips it (`if: matrix.slug != 'macos-x86_64'`), so that
# asset is published without having been run.
set -euo pipefail

[ $# -eq 1 ] || { echo "usage: smoke.sh <markdoc-lint binary>" >&2; exit 2; }
[ -f "$1" ] || { echo "::error::no markdoc-lint binary at $1" >&2; exit 1; }
BIN="$(cd "$(dirname "$1")" && pwd)/$(basename "$1")"
REPO="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
EXAMPLE="$REPO/examples/markdoc-lint.toml"
[ -f "$EXAMPLE" ] || { echo "::error::no $EXAMPLE" >&2; exit 1; }

work="$(mktemp -d)"
trap 'rm -rf -- "$work"' EXIT
# Every path handed to the binary below is relative to here. Git Bash
# rewrites an absolute POSIX path passed to a Windows program, and the
# directory that arrives is not always the one that was meant.
cd "$work"

failures=0
fail() { echo "::error::$*" >&2; failures=$((failures + 1)); }

# has FILE TEXT WHY / lacks FILE TEXT WHY -- fixed strings, not patterns,
# so a `[` or a `.` in a rule's output means itself.
has()   { grep -qF -- "$2" "$1" || fail "$3: expected $1 to contain: $2"; }
lacks() { if grep -qF -- "$2" "$1"; then fail "$3: expected $1 not to contain: $2"; fi; }

# run NAME ARGS... -- the binary's stdout to NAME.out, stderr to NAME.err,
# exit status to $code. Never fatal: the status is what is being checked.
run() {
  local name="$1"; shift
  set +e
  "$BIN" "$@" > "$name.out" 2> "$name.err"
  code=$?
  set -e
  echo "--- markdoc-lint $* -> exit $code"
  cat "$name.out" "$name.err"
}

# ── 1. A fault the language defines ─────────────────────────────────────
#
# No convention configured: an empty markdoc-lint.toml, so the run is
# "configured" and the two language rules are all that can fire.
mkdir language
: > language/markdoc-lint.toml
printf '# Fine\n\nA paragraph.\n' > language/ok.md
printf '# Not fine\n\n{%% callout %%}\nbody\n' > language/bad.md

run language-json --json --root language
# Exit 1 is "found an error", and it is what a hook and a CI step both read.
# A green run on a corpus with a broken tag in it is the failure this exists
# to catch.
[ "$code" = 1 ] || fail "markdoc-lint --json exited $code on a corpus with an unclosed tag"
has   language-json.out '"files": 2,'                'both documents were read'
has   language-json.out '"configured": true'         'the config file was found'
has   language-json.out '"rule": "unclosed-tag"'     'the unclosed tag was reported'
has   language-json.out '"file": "bad.md"'           'against the file that has it'
lacks language-json.out '"file": "ok.md"'            'and not against the one that does not'

# The text form is the one a terminal and an editor's problem matcher read:
# findings on stdout as path:line:col, the summary on stderr.
run language-text --root language
[ "$code" = 1 ] || fail "markdoc-lint exited $code on a corpus with an unclosed tag"
grep -q '^bad\.md:3:1: error\[unclosed-tag\] ' language-text.out ||
  fail "stdout has no 'bad.md:3:1: error[unclosed-tag]' line"
has   language-text.err '2 file(s) linted against an index of 2; 1 error(s), 0 warning(s).' \
      'the summary is on stderr'

# ── 2. The worked example, as the README shows it ───────────────────────
#
# examples/markdoc-lint.toml verbatim, and one document or file per thing
# it turns on, so each section is seen doing what the README says it does.
mkdir -p example/.tooling example/products/widget/manual example/products/widget/editions
cp "$EXAMPLE" example/markdoc-lint.toml
# A tag of the corpus's own. If the overlay is not loaded, `{% spec %}` is
# reported as undefined -- which is the check that it was.
cat > example/.tooling/tags.overlay.toml <<'TOML'
[tags.spec]
description = "One row of a product's specification"
self_closing = true

[tags.spec.attributes.name]
type = ["String"]
required = true
TOML
printf '{"company": "Acme Robotics"}\n' > example/global.config.json
# `[anchors] declare_tag` makes the `=` typo its own finding, with what
# upstream Markdoc says of it (an annotation giving the heading an attribute
# it does not declare) suppressed; `[hardcoded-variables]` reports the
# company name typed out; and this directory holds a page but no nav.json,
# which `[[layout]]` requires.
cat > example/products/widget/manual/safety.md <<'MD'
# Safety

Acme Robotics builds the widget.

{% spec name="Payload" /%}

## Docking {% tag="docking" %}

Prose.
MD
# `[[frontmatter.files]]` reaches an untyped edition, and its pattern refuses
# a version YAML read as a number. `[rules.frontmatter]` re-levels that to a
# warning.
printf -- '---\ntitle: Widget 1.10\nswVersion: 1.10\n---\n\nThe edition.\n' \
  > example/products/widget/editions/1.10.md

run example-json --json --root example
[ "$code" = 1 ] || fail "markdoc-lint --json exited $code under the worked example, which has errors to find"
has   example-json.out '"files": 2,'                           'both documents were read'
has   example-json.out '"configured": true'                    'examples/markdoc-lint.toml was accepted'
has   example-json.out '"rule": "malformed-anchor"'            '[anchors] declare_tag'
lacks example-json.out '"rule": "undefined-attribute"'         'malformed-anchor suppresses the generic report'
has   example-json.out '"rule": "hardcoded-variable"'          '[hardcoded-variables] sources'
has   example-json.out '"rule": "missing-file"'                '[[layout]]'
has   example-json.out '"rule": "invalid-frontmatter-value"'   '[[frontmatter.files]] patterns'
lacks example-json.out '"rule": "undefined-tag"'               '[schema] overlay was loaded'
lacks example-json.err 'warning:'                              'the overlay was read without complaint'

run example-text --root example
[ "$code" = 1 ] || fail "markdoc-lint exited $code under the worked example, which has errors to find"
grep -q '^products/widget/manual: error\[missing-file\] ' example-text.out ||
  fail "stdout has no 'products/widget/manual: error[missing-file]' line"
grep -q '^products/widget/editions/1\.10\.md:1:1: warning\[invalid-frontmatter-value\] ' example-text.out ||
  fail "[rules.frontmatter] severity = \"warning\" did not re-level invalid-frontmatter-value"

# A finding about a directory is not about a file somebody named, so naming
# files skips the corpus rules -- and still reads the whole root for the
# index.
run example-paths --json --root example example/products/widget/manual/safety.md
[ "$code" = 1 ] || fail "markdoc-lint --json exited $code on a named file with a malformed anchor"
has   example-paths.out '"files": 1,'                        'only the named file was linted'
has   example-paths.out '"indexed": 2,'                        'against an index of the whole root'
has   example-paths.out '"rule": "malformed-anchor"'           'the named file was linted'
lacks example-paths.out '"rule": "missing-file"'               '[[layout]] runs only over the whole corpus'

# ── 3. markdoc.toml, how markdoc reads the corpus ────────────────────────
#
# Read after the overlay, from the content root: its [parse] options are the
# ones every document is parsed with, and [validation] with [functions]
# says which functions a page may call.
mkdir options
: > options/markdoc-lint.toml
cat > options/markdoc.toml <<'TOML'
[parse]
allow_indentation = true

[validation]
validate_functions = true

[functions.higher]
description = "A version at or above a threshold"
[functions.higher.parameters.0]
type = ["String"]
required = true
TOML
# A callout indented four spaces, which is a tag only with allow_indentation
# (without it the lines are text, and the two tags pair with nothing); a call
# to a function nobody declared, and one to a declared function without its
# argument.
printf -- 'Text.\n\n    {%% callout %%}\n    body\n    {%% /callout %%}\n\n{%% if nosuch(1) %%}\nx\n{%% /if %%}\n\n{%% if higher() %%}\ny\n{%% /if %%}\n' \
  > options/page.md

run options-json --json --root options
[ "$code" = 1 ] || fail "markdoc-lint --json exited $code on a page calling an undeclared function"
has   options-json.out '"files": 1,'                         'the page was read'
has   options-json.out "Undefined function: 'nosuch'"        '[validation] validate_functions'
has   options-json.out "Missing required parameter: '0'"     '[functions.higher] parameters'
lacks options-json.out '"rule": "unclosed-tag"'              '[parse] allow_indentation'
lacks options-json.err 'warning:'                            'markdoc.toml was read without complaint'

if [ "$failures" -gt 0 ]; then
  echo "::error::$failures smoke check(s) failed" >&2
  exit 1
fi
echo "==> markdoc-lint finds what it should, in text and in JSON"

#!/usr/bin/env bash
#
# Everything that has to be true before markdoc-lint is published.
#
# ONE COPY, as in tw: ci.yml runs it on every push and pull request,
# release.yml runs it before a tag publishes anything. A second list of steps
# drifts, and the half that drifts is always the one nobody watches.
#
# markdoc-lint is hosted on GitHub and built there for all four platforms it
# ships, so there is one workflow directory and no Forgejo half to keep in
# step with this.
#
# Inputs:
#   $1          the markdoc-lint checkout              default: $PWD
#   STRICT      1 to fail on clippy warnings           default: 1
set -euo pipefail

ROOT="${1:-$PWD}"
cd "$ROOT"
ROOT="$PWD"
STRICT="${STRICT:-1}"

group() { echo "::group::$*"; }
endgroup() { echo "::endgroup::"; }
die() { echo "::error::$*" >&2; exit 1; }

[ -f Cargo.toml ] || die "no Cargo.toml in $ROOT — point this at the markdoc-lint checkout"

failed=""
step() {
  local name="$1"; shift
  group "$name"
  if "$@"; then
    endgroup
  else
    endgroup
    echo "::error::$name failed"
    failed="$failed $name"
  fi
}

# Formatting first: it is the cheapest, and a reformat in the middle of a
# clippy fix is a confusing diff to review.
step "cargo fmt" cargo fmt --check

if [ "$STRICT" = "1" ]; then
  step "cargo clippy" cargo clippy --all-targets -- -D warnings
else
  step "cargo clippy" cargo clippy --all-targets
fi

step "cargo test" cargo test --all-targets

# The release profile is what ships, and it differs from debug in more than
# speed: an overflow that panics in debug wraps in release.
step "cargo build --release" cargo build --release

# A binary that will not start is worse than one that was never built: the
# consumer caches it and reports a tool that is "installed".
step "it runs" ./target/release/markdoc-lint --version

# And it finds things. Two corpora built on the spot, one of them under
# examples/markdoc-lint.toml, with the exit code, the JSON and the text
# form all checked -- see smoke.sh for why not a fixture directory.
step "it finds faults in a corpus" bash .github/scripts/smoke.sh ./target/release/markdoc-lint

for s in .github/scripts/*.sh; do
  step "bash -n $(basename "$s")" bash -n "$s"
done

if [ -n "$failed" ]; then
  die "failed:$failed"
fi
echo "==> all checks passed"

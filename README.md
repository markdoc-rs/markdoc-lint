# markdoc-lint

Lint rules for a [Markdoc](https://markdoc.dev) corpus, over the real AST. A
library and a CLI.

## Why an AST and not regular expressions

Most checks of this kind are written as regular expressions over the file
text, and they all end up re-implementing a parser badly in order to avoid
one. A rule that wants to know whether a `{% tagref %}` resolves has to skip
tags inside fenced code blocks — and the page documenting the convention is
full of them. A rule about variables inside `{% if %}` has to track nesting
to know which `{% /if %}` closes which `{% if %}`.

The parser already knows both. A rule here asks the tree a question, which
also means a diagnostic can say *where*: every node carries its source
position, so an editor can jump to the fault.

The parser is `markdoc`, the port of upstream Markdoc 0.5.11, with the
extensions in `markdoc::ext`: the same tree, the same tags and the same
pairing the renderers and the editor get, because every tool reading a corpus
parses it with the corpus's own `[parse]` options (see [The index](#the-index)).
Upstream processes the tags in a fenced block unless it says `process=false`;
the convention rules still treat a fence's content as an example, and skip it.

## Two kinds of rule, and why the split decides everything else

Everything a Markdoc document can get wrong divides in two.

**Faults the language defines.** A tag that never closes; an attribute the
schema does not allow. `syntax` and `schema` check these, they need no
configuration, and they run unless a corpus switches them off.

**Faults a project defines**, which is every interesting one.
`{% tagref "x" %}` is a perfectly valid tag whatever `x` is; that it must
name an anchor declared somewhere a page can reach is a thing *one corpus*
decided. The same goes for which directory partials live in, what their
filenames begin with, where variables come from, which frontmatter fields a
`faq` requires, which values a page should never type out, and what a
directory has to hold.

Hard-coding those answers is how a linter becomes un-adoptable: the rules
are useful, the values are not transferable, and the second project forks the
crate to change nine string literals. So the values are data. `anchors`,
`partials`, `variables`, `frontmatter`, `hardcoded-variables` and `layout`
each return immediately until `markdoc-lint.toml` configures them.

**The default is to say nothing.** With no config file you get the two
language-level rules and silence from the rest. A rule that fires on every
file of a corpus it was not configured for teaches people to pass
`--no-verify`, and an editor that opens one loose `.md` has no project to
read conventions from.

## The rules

Two vocabularies, tied together by this table. The first column is what a
finding is called — its `rule` field in JSON, the word in brackets in the
text form, the thing people grep a CI log for. The second is the rule that
reports it: the name `[rules.…]` takes, and the name of the section that
configures it, where there is one.

| reported as | rule | severity | needs |
|---|---|---|---|
| `unparsable` | `syntax`, or none | error | — |
| `unclosed-tag`, `unmatched-closing-tag`, `syntax` | `syntax` | from the parser | — |
| `malformed-anchor` | `syntax` | error | `anchors.declare_tag` |
| `missing-attribute`, `invalid-attribute-type`, `invalid-child`, `undefined-attribute`, `schema` | `schema` | from the validator | — |
| `undefined-tag` | `schema` | warning | — |
| `anchor-without-id`, `duplicate-anchor` | `anchors` | error | `anchors.declare_tag` |
| `reference-without-id`, `unresolved-reference` | `anchors` | error | `anchors.reference_tag` + an index |
| `partial-without-file` | `partials` | error | `partials.dir` |
| `missing-partial` | `partials` | error | `partials.dir` + an index |
| `partial-naming` | `partials` | warning | `partials.dir` + `partials.prefixes` |
| `unknown-variable` | `variables` | error | `variables.sources` + an index |
| `unknown-doc-type` | `frontmatter` | error | `frontmatter.type_field` and at least one type |
| `missing-frontmatter-field`, `invalid-frontmatter-value` | `frontmatter` | error | the same, or a `[[frontmatter.files]]` that matches |
| `hardcoded-variable` | `hardcoded-variables` | warning | `hardcoded-variables.sources` |
| `missing-file` | `layout` | error | a `[[layout]]` with a `require`, in a run over the whole corpus |

The parser and the validator are upstream Markdoc's, and their ids are
mapped onto this crate's, because an id is a thing people grep for and put in
an ignore list. It outlives the code that emits it and may not be renamed for
tidiness:

| upstream | reported as |
|---|---|
| `missing-closing` | `unclosed-tag` |
| `missing-opening` | `unmatched-closing-tag` |
| `frontmatter-invalid` | `unparsable` |
| any other error of the parser's (`parse-error`, `duplicate-attribute`, `no-inline-annotations`, ...) | `syntax` |
| `attribute-missing-required` | `missing-attribute` |
| `attribute-type-invalid` | `invalid-attribute-type` |
| `child-invalid` | `invalid-child` |
| `attribute-undefined` | `undefined-attribute` |
| `tag-undefined` | `undefined-tag` |
| any other id of the validator's (`tag-placement-invalid`, `tag-selfclosing-has-children`, `attribute-value-invalid`, `function-undefined`, ...) | `schema` |

`syntax` and `schema` keep upstream's message for the ids they do not name;
the rest say it in this crate's words, and name the tag.

Severity comes from the level upstream gives — critical and error become
`error`, warning becomes `warning`, debug and info become `hint` — with one
exception. `undefined-tag` is a warning, where upstream's `tag-undefined` is
critical: the tag renders as its content only, its framing and attributes
dropped, and the fix is to declare it, which is an edit to a schema file
rather than to every page that uses it. A tag written inline where its schema
says block (`tag-placement-invalid`), or with children where it says
self-closing, is an error: the renderers honour the declaration, and the
schema data says where each tag goes.

`unparsable` is a document whose frontmatter is not YAML: the parser keeps it
as text, and every rule that reads it as data goes quiet. `syntax` reports
it. The parser refuses a document outright only where upstream throws —
nesting past a depth limit, or a tag given both `.class` and `class=` — and
that is reported before any rule runs, under no rule, so `[rules.…]` can
neither switch it off nor re-level it.

### `malformed-anchor`, which is the reason this rule set exists

`{% tag="x" %}` — an `=` where a space belongs. Markdoc reads
`{% key=value %}` as an *annotation*: attributes for the block it ends. So the
heading gets an attribute `tag` that nothing reads, no anchor is declared,
and the page still renders perfectly. Every reference aimed at that heading
has been resolving to nothing, silently. On a line of its own it applies to
nothing at all, and written self-closing, `{% tag="x" /%}`, the grammar
cannot read it.

What upstream says of it is true and no help: "Invalid attribute: 'tag'",
"Can't apply inline annotations to 'document'", or a grammar error at the
`/`. So this rule reports the typo with advice that works — a space and no
`=` — at the annotation itself, and **suppresses** those reports.

Only the configured anchor tag is checked, because only there is the intent
unambiguous: `{% key=value %}` with any other key is an annotation somebody
may mean.

It is `syntax` that reports it, not `anchors`, because what it suppresses is
the parser's and the schema's report of the same characters.
`[rules.anchors]` leaves it alone; `[rules.syntax]` takes it along with the
rest.

### One fault, one line of output

- `validate` walks the tree and re-emits every `node.errors` it finds on the
  way, so `schema` drops the ids the parser already reported. Without that
  every syntax fault appeared twice — once as itself and once as a schema
  error, on the same line, saying the same thing.
- A tag left open inside a paragraph or a table cell leaves the Markdown
  around it open too, and upstream reports those nodes ("Node 'inline' is
  missing closing"), not the tag. `syntax` reads the pairing from the
  tokenizer instead (`markdoc::ext::occurrences`), names the tag, and does
  not report the table it broke, or the closing tag that then closed
  nothing, as faults of their own; `schema` leaves alone the blocks the
  unclosed node swallowed ("Can't nest 'paragraph' in 'inline'").
- A closing tag with nothing open to close becomes a node of its own. It is
  reported once, as `unmatched-closing-tag`, and not validated as a tag.
- Whether a `{% partial %}` names a file that exists is the `partials`
  rule's question, answered against the index. `schema` does not ask it
  again: upstream looks the file up in `config.partials`, which the lint,
  having no partials to give it, leaves empty, and its "Partial `x` not
  found" is dropped.

### Switching a rule off, or re-levelling it

```toml
[rules.frontmatter]
severity = "warning"      # error, warning or hint

[rules.layout]
enabled = false
```

Two knobs, keyed by the rule — the second column above — and deliberately
not a policy language. `enabled = false` is for a rule a corpus has not
adopted yet. `severity` is for one it has adopted but is not ready to be
stopped by: it replaces the severity of *every* finding the rule produces,
so a rule that reports both errors and warnings loses that distinction,
which is the point — the corpus has decided how much the rule matters. Both
are applied by the code that runs the rules rather than by the rules, so no
rule can forget to honour them, and the exit status follows the severity
after the override: a rule re-levelled to `warning` cannot fail a build.

The key is a rule, not an id, so `[rules.syntax]` moves `unclosed-tag` and
`malformed-anchor` together. A key that is not a rule is an error that lists
the ones that are: a misspelled override that was silently ignored would be
the failure an unknown-key error exists to prevent, one level down.

## The index

Most of the rules ask something one document cannot answer: whether a
reference names an anchor in another page, whether a `{% partial %}` names a
file, whether a `{% $variable %}` names a key in a config, which values a
page should have written as a variable, which tags the corpus declares, and
what its directories hold. That is answered from a `Corpus` built once.

It is cheap to build — a parse of each file, which the linter was going to
do anyway — and expensive to get wrong. An index that is a little bit stale
reports a broken reference to a thing that is actually there, which is the
fastest way there is to teach a writer to ignore the linter.

| in the index | built from |
|---|---|
| anchors, per scope | every non-partial `.md`, first declaration wins |
| partial paths | every `.md` under `partials.dir` |
| variable names | the JSON files matching `variables.sources`, `variables.extra`, and frontmatter keys |
| literals | the top-level string values of the JSON files matching `hardcoded-variables.sources` |
| the tag schema | markdoc's bundle, plus the file `schema.overlay` names, plus `markdoc.toml` |
| the parse options | `[parse]` in those files, with YAML frontmatter on |
| a file count | what was read, so the rules can tell an empty index from a clean corpus |

**An empty index means silence, not accusation.** `is_indexed()` is false
when nothing was read, and the cross-document rules return rather than
reporting every reference in the file as broken. That is exactly the state a
language server is in when a writer opens one `.md` out of a directory.

**A partial declares no anchors.** It is included into pages in several
scopes, so an anchor in one would be declared once per inclusion. In the
other direction a partial may *reference* an anchor in any scope, because it
cannot know which page it will land in; every other file sees only its own
scope.

**Variable names are collected at every depth.** `{"safety": {"pl_d": "PL
d"}}` makes both `safety` and `safety.pl_d` usable, because
`{% $safety.pl_d %}` and `{% $safety %}` are both things a page writes. Keys
beginning `//` are skipped — a common comment convention in JSON config
files. Frontmatter keys count when `from_frontmatter` is on, which is the
default, and are gathered in the same pass that collects anchors — so, as
things stand, only when `anchors.declare_tag` is set too.

**The tag schema is built once**, from markdoc's bundle
(`markdoc::ext::bundle`, the crate's `schema/tags.toml`), the overlay
`[schema]` names — tags this corpus has and Markdoc does not, in markdoc's
own tag-schema format, layered field by field over the bundle — and then
`markdoc.toml` at the content root, if there is one. Every tool that reads a
corpus has to load the same files. One that does not sees the corpus's own
tags as undeclared, which is what `undefined-tag` reports.

**`markdoc.toml`** says how markdoc reads the corpus: `[parse]`, the options
every tool parses it with (`allow_indentation`, `allow_comments`,
`strip_bom`, ...), so that the lint, the editor and the renderers agree on
what is a tag; `[render]`, which the lint does not use (the typographer
would move text positions); `[validation] validate_functions`; and
`[functions.<name>]`, the functions the corpus's pages call beyond
upstream's six, with their parameters. With `validate_functions` on, a call
to a function nobody declared is reported (`schema`, "Undefined function")
instead of rendering as nothing. The overlay may carry `[parse]` too; a
value in `markdoc.toml` wins. The lint always reads frontmatter as YAML,
which is what its frontmatter rule and the variable index need and which
changes nothing about tags.

### Scopes

`Global` is one namespace for the corpus. `PathPrefix` gives each subtree its
own, which is what a corpus documenting several products wants: two products
may both declare `safety_contactors` and mean different pages, and a
reference that silently resolved to the other product's page would be worse
than one that failed.

```toml
scope = { kind = "path-prefix", under = "products", depth = 1 }
```

`depth` counts segments *after* `under`, so `products/widget/manual/a.md`
scopes to `products/widget` and a product's variants can cross-reference
each other. Anything outside the prefix, or with too few segments under it,
falls into the global namespace — whose name is `<global>`, not a path, so it
cannot collide with one.

## Configuration

`markdoc-lint.toml` at the content root. See
[`examples/markdoc-lint.toml`](examples/markdoc-lint.toml) for a worked one.
The shape:

```toml
[schema]
overlay = ".tooling/tags.overlay.toml"  # omit for the built-in tags only

[corpus]
# Defaults to node_modules/**, .* and .*/**.
skip = ["README.md", ".tooling/**", "translations/**"]

[partials]
dir = "partials"                        # omit to disable the partial rules
tag = "partial"
file_attributes = ["file", "primary"]   # tried in order
prefixes = ["text", "list", "table"]    # empty to disable the naming check

[anchors]
declare_tag = "tag"                     # omit to disable the anchor rules
reference_tag = "tagref"
id_attributes = ["primary", "id"]       # tried in order
scope = { kind = "path-prefix", under = "products", depth = 1 }

[variables]
sources = ["global.config.json", "*_config.json"]
from_frontmatter = true
extra = []                              # names a renderer injects

[hardcoded-variables]
sources = ["global.config.json"]        # omit to disable the rule
skip_kinds = ["em", "strong", "code", "fence"]   # the default
skip_tags = ["image"]

[frontmatter]
type_field = "type"                     # or ["doc_type", "type"]; omit to disable
required = ["id", "title", "status"]
aliases = { release_note = "releaseNote" }
enums = { status = ["draft", "published"] }
patterns = { version = '^[0-9]+(\.[0-9]+)*$' }

[frontmatter.types.faq]
required = ["question"]
enums = { category = ["safety", "product"] }   # for a faq only
patterns = { question = '\?$' }                # for a faq only

[[frontmatter.files]]                   # by path, typed or not
match = "products/*/editions/*.md"
required = ["title", "swVersion"]
patterns = { swVersion = '^[0-9]+\.[0-9]+(\.[0-9]+)?$' }

[[layout]]                              # one per convention
dir = "products/*/manual"
when_contains = "*.md"                  # optional
require = "nav.json"

[rules.frontmatter]                     # by rule; both keys optional
enabled = true
severity = "warning"
```

The frontmatter type checks need **both** a `type_field` and at least one
`[frontmatter.types.*]`: a type field with no types to check against has
nothing to say. `type_field` may be a list, tried in order, for a corpus
that writes the type under more than one name: a document whose type is
under a name the rule was not told about is not checked at all, and says
nothing. `[[frontmatter.files]]` needs neither — it is how a corpus
says something about documents its type system cannot reach.

An unknown key is an error rather than a silent no-op, because a typo in a
config that still parses is a rule that stops firing without anyone
noticing. The same holds one level down: `[rules.…]` is checked against the
rule names, and a `patterns` value that is not a valid regular expression is
refused when the file is read, naming where it was written, rather than
skipped when a document is checked. A *malformed* file is likewise an error,
where a *missing* one is the defaults — falling back to the defaults on a
parse failure would turn a typo into "the linter stopped finding things",
which is the failure mode hardest to notice.

The overlay is the exception. A `[schema] overlay` that cannot be read or
does not parse is reported on stderr, and the run goes on with the built-in
tags — so wherever a page uses one of the overlay's tags, `undefined-tag`
reports it instead: the run still says something true, only louder.
`undefined-tag` is a warning, so on its own that does not change the exit
status.

The globs are hand-rolled: `*` matches within a path segment, `**` across
them, and `a/**/b` matches `a/b`. A glob matches the whole corpus-relative
path, so `skip`, `[[frontmatter.files]] match` and `[[layout]] dir` are
anchored at the root; the two `sources` lists are tried against a file's
name as well, so `*_config.json` finds a config at any depth. The whole of
the need is a literal, a prefix and a suffix, and a glob dependency in a
language server is a dependency a writer installs.

`skip` replaces its default rather than adding to it. Whatever it says, the
walk never enters a directory named `node_modules` or `target`, or anything
whose name begins with `.`.

## Rules that are easy to get wrong, and how they behave

**`unknown-variable`** counts a name as known if it, or any dotted prefix of
it, is defined. `$robot.name` is fine when `robot` is an object: the config
supplies the object and the page reaches into it, and checking only the full
dotted path would report every field access in the corpus. A variable is
found wherever it is written: a tag's attribute, a function's argument, a
list, or the text node `{% $name %}` becomes. `$a.b[0]` is `a.b.0`. A
half-typed `{% $robot. %}` is no variable but a tag the grammar cannot read,
which `syntax` reports.

**`missing-frontmatter-field`** and its siblings skip partials and skipped
files, and say nothing when a document has no frontmatter at all —
`[[frontmatter.files]]` included. Pages that inherit it from a parent
legitimately have none, and deciding which those are needs the navigation,
which is a corpus question rather than a document one. These three report at
line 1: the fault is the absence of text, so there is no text to point at.

**A frontmatter pattern** is a regular expression searched for in the
trimmed value — `[0-9]+\.[0-9]+` accepts `v2.3 and later` — so anchor it
with `^` and `$` to say what all of the value must look like. A field with a
pattern has to be a quoted string, and an unquoted one is reported as that
rather than as a mismatch: YAML reads `swVersion: 1.10` as the number 1.1, so
by the time the rule sees it the digit is gone, and "does not match" would
send the writer looking for a typo that is not there. A field left empty
(`swVersion:`, which YAML reads as null) is not checked, and an absent one is
`required`'s business. Requirements add up: a typed document gets the global
`required`, `enums` and `patterns` and its type's, plus those of every
`[[frontmatter.files]]` whose `match` fits it, and a missing field or a value
of the wrong shape says which type or glob asked for it.

**`hardcoded-variable`** finds a value from `hardcoded-variables.sources`
typed out where the variable that renders it belongs. It matches whole words
only — `R100` inside `R1000` is not a match — and tries the longest value
first, so a product called `R100` cannot shadow one called `R100 Lift`. In
running text the advice is `{% $key %}`; inside a tag's attributes or its
condition it is the bare `$key`, which is the syntax there. A value two keys
share offers both. Frontmatter is never read, because it is where the values
come from; neither is anything inside a node `skip_kinds` names (Markdoc's
node names; `em`, `strong`, `code` and `fence` unless set) or a tag in
`skip_tags`, which is skipped whole, attributes and content. Inside a tag
the values are read wherever they are nested: a function's arguments
(`{% if equals($x, "Bot600") %}`), lists and objects. In running text,
table cells included, the finding points at the literal; inside a tag, at
the tag.

Its `sources` are separate from `[variables]` on purpose. `[variables]`
asks whether a name is defined *anywhere*, so it is generous: every config
file that may define a name a page uses. This asks whether some text *should
have been* a variable, which is only true of values that hold corpus-wide —
one product's config naming its model does not make that name a mistake on
another product's page. Only top-level strings count: a nested object's
leaves are structure, and a number is not text anybody types by mistake.

**`missing-file`** is about a directory, not a document. `[[layout]] dir` is
matched against every directory the walk reaches except the root and what
`skip` covers. `when_contains` and `require` are matched against every file
under that directory, at any depth and of any kind, by its path relative to
the directory — so `*.md` is a direct child and `**/*.md` is anywhere
beneath. Without `when_contains` an entry is about every directory `dir`
matches; with it, only about the ones already holding such a file, which is
the difference between "every manual has navigation" and "a manual with
pages in it has navigation". An entry without `require` checks nothing. The
finding names the directory and has no line to point at, and it is looked
for only in a run over the whole corpus.

**`undefined-tag`** is upstream's `tag-undefined`, as a warning. An alias
counts as declared, and `{% $name %}` is no tag at all (a text node whose
content is the variable). When a declared name is close enough to be a
typo, it says which.

**`duplicate-anchor`** is per file, and names the line the first declaration
is on. The later one wins silently, so every reference to that id lands on
whichever heading happens to come last.

**`missing-partial`** resolves a path relative to `partials.dir`, which is
how they are written — and so `file="common//text_a.md"`, with the doubled
slash, is a missing partial rather than a path that happens to work.

## Using it

```
markdoc-lint [--json] [--root <dir>] [path…]
```

With no paths, every `.md` under the root, and the corpus rule — `layout` —
once: the corpus rule's findings come first, then each document's, in path
order. With paths, only those, in the order they are named — a directory
means every `.md` under it, in path order — and no corpus rule: a finding
that some directory is empty is not about the file somebody named. Either
way the index is built from the whole root, because whether an anchor
exists somewhere else cannot be answered from the file being linted. Both
the index and the documents are parsed on every core the machine has, and
the output is in the same order whatever finishes first.

```
$ markdoc-lint --root docs-content
products/widget/overview.md:7:5: error[unresolved-reference] `emergency_stop` is not an anchor this page can reach.
    help: An anchor is `{% tag "emergency_stop" /%}` on the target heading. This page sees the anchors of `products/widget`.
products/widget/safety.md:9:19: error[malformed-anchor] This `{% tag %}` is an opening tag, so everything after it on the page became its content.
    help: An anchor is self-closing and takes its id as an unkeyed value: write `{% tag "some_id" /%}`, with a space and no `=`. As written it declares no anchor at all, so every reference aimed at it resolves to nothing.

3 file(s) linted against an index of 3; 2 error(s), 0 warning(s).
```

Two findings and one fault: the heading `overview.md` points at declares
nothing, because its anchor was written with an `=`.

Findings go to stdout and the summary to stderr, so a pipe gets only the
findings. The text form is `path:line:col: severity[rule] message` — the
shape every compiler and editor already knows how to jump to — with `help`
on its own indented line. A finding with no position, such as a
`missing-file` about a directory, is `path: severity[rule] message`.

`--json` prints **one object**, not a stream: `files`, `indexed`,
`configured`, `errors`, `warnings`, `diagnostics`. A caller that wants to
count has to read the whole thing anyway, and a half-written stream after a
crash looks exactly like a clean run. Each diagnostic has `file`,
`severity`, `rule`, `message`, `span` and `help`; `span` is the six fields of
a `Span`, or `null` where there is no position. A hint is in `diagnostics`
and in neither count.

Exit status is 1 if anything was reported at error level, after
`[rules.…]` overrides, and 0 otherwise. A warning does not fail a build,
because a writer who cannot merge until every style hint is gone stops
reading them. It is 1 as well when there is nothing to lint with — an
unknown option, a `--root` that does not exist, a config file that does not
parse — and stderr says which.

When there is no `markdoc-lint.toml` at the root the summary says so, and
`--json` says `"configured": false`. Otherwise a corpus with a misplaced
config file looks clean.

`--version` prints the version, and `-h` or `--help` the usage.

### As a library

```rust
use markdoc_lint::{Config, Corpus, lint_corpus, lint_document};
use std::path::Path;

let root = Path::new("../docs-content");
let config = Config::load(root)?;             // defaults if there is no file
let corpus = Corpus::load_with(root, config); // the index, built once

let linted = lint_document("products/widget/safety.md", &source, &corpus);
for d in &linted.diagnostics {
    println!("{}", d.to_text());
}

// The rules about the corpus's shape: once, and only for the whole of it.
for d in lint_corpus(&corpus) {
    println!("{}", d.to_text());
}
```

`lint_document` returns the parse alongside the diagnostics, because every
caller wants it: a CLI to decide an exit code, a language server to answer
the hover that follows. `lint_corpus` is separate because a caller checking
named files does not want it — a finding about an empty directory is not
about the file somebody staged, and a hook that reported it would be
reporting somebody else's work. `Corpus::load` reads the config itself, and
falls back to the defaults if it does not parse; `load_with` takes one the
caller has already validated, which is how the CLI reports a malformed
config as an error instead of quietly linting with the defaults.

`lint_document`'s diagnostics are ordered by position and then by rule, and
`lint_corpus`'s by path and then by rule. The CLI, run over the whole
corpus, prints the corpus rule's findings first, then each document's, in
path order. Two runs over the same corpus print the same thing, or a diff of
CI output is noise.

A `Span` carries line, UTF-16 column *and* byte offset, because each caller
wants a different one and converting is not free: an editor counts UTF-16
columns, a terminal prints line and column, a fixer slices bytes. `help` is
separate from `message` because an editor shows the message in a tooltip and
the help only when asked, and because a message that ends in advice is harder
to read when it fires 200 times.

`walk`, `tag_name`, `str_attr` and `first_str_attr` are public for the same
reason — a language server asks the same questions of the same tree.

That tree carries markdoc's own positions (`node.span`, and the `span` of its
errors), which markdoc computes only when a parse asks for them. The lint's
parses ask — `lint_document` and the anchor index, since every finding and
every anchor's line is read from them — and `Corpus::parse_options` does
not: it is the corpus's options, which every tool shares and most use
without positions. A tool that parses with it and wants positions asks for
them itself: `corpus.parse_options.with_spans(true)`.

A `Diagnostic` deserialises as well as serialises, so a tool that reads
another tool's `--json` gets this type back rather than a second definition
of the format. Its `rule` is a `Cow<'static, str>`: a built-in id is a
literal and costs nothing, an id read from somewhere else is owned rather
than leaked, and either way it serialises as a plain string, so the wire
format is the one it always was.

## What a rule is

A type implementing `Rule`. It describes itself — `describe()` returns its
name, a one-line summary, every id it can report, the worst severity it
reports with no override in force, and the configuration that turns it on —
and `check()` takes one document's tree, its corpus-relative path and the
`Corpus`, and pushes `Diagnostic`s. `rules::ALL` is the set, in the order
they run; `lint_document` walks it, skips a rule the corpus switched off and
re-levels what an overridden one produced. A rule about the corpus rather
than a document is a `CorpusRule` — handed no tree, run once by
`lint_corpus`, listed in `rules::CORPUS_RULES` — and describes itself the
same way, so both kinds list, configure and override alike.

There used to be no trait here. Six free functions called from one place is
a list you can read in ten seconds, and the argument against a trait — that
it would buy dynamic dispatch nobody needs, over a set that changes once a
year — was right about dispatch. What a list of calls cannot do is answer
questions about itself. Every convention rule is silent until configured, so
a corpus that never turned a rule on and a corpus with nothing to report
print exactly the same thing: nothing. `rules::status(&config)` answers it:
for each rule, whether it will run, whether the corpus configured it,
switched it off or re-levelled it, and which configuration would turn it on.
It lives here rather than in a front end so that every front end answers the
same way.

Each is named for the thing it protects, not for the mechanism — `anchors`,
not `tagref_regex` — because the mechanism is the part most likely to be
replaced.

## Building

`markdoc` — the parser — comes from crates.io, at the version `Cargo.toml`
asks for and `Cargo.lock` pins. What this crate uses of it: the conformant
port of upstream Markdoc at the crate's root paths (`markdoc::parse`,
`markdoc::validate`, ...), and `markdoc::ext`, the opt-in layers — the
schema bundle and overlays (`ext::bundle`, which `[schema] overlay` and
`markdoc.toml` are loaded through), the parse options (`ext::ParseOptions`)
and the tags the tokenizer recognised (`ext::occurrences`). The rest of the
family takes the same release: one parser behind the website, the PDFs, the
linter and the editor, or they disagree about what a document means and the
preview stops being evidence. `markdoc-ls` and `markdoc-prose` depend on
this crate by path, as `../markdoc-lint`.

```
cargo build --release
```

To build against an unreleased markdoc in a checkout beside this one, add
`[patch.crates-io] markdoc = { path = "../markdoc" }` to `Cargo.toml`, and
take it out before committing: CI has no `../markdoc`.

```
bash .github/scripts/check.sh
```

is the gate CI runs, and it runs the same here: formatting, clippy with
warnings denied, the tests, a release build, and that build run over two
corpora it has to find faults in (`.github/scripts/smoke.sh`). A tag
publishes `markdoc-lint-<platform>` for Linux (x86-64, static), Windows
(x86-64) and macOS (Apple Silicon and Intel), each with a `.sha256` beside
it.

## Status

- [x] Two language rules that need no configuration; five convention rules
      over documents and one over the corpus's directories, each silent
      until it has some
- [x] One `lint_document` for the CLI and the language server, and one
      `lint_corpus` for a run over the whole corpus
- [x] Deterministic output, both text and JSON
- [x] `malformed-anchor`, with upstream's reports of the same characters
      suppressed
- [x] Upstream Markdoc's parser and validator (markdoc's port), with the
      corpus's own parse options, and upstream's ids under the lint's names
- [x] Rules describe themselves, and a corpus can switch one off or
      re-level it
- [x] Tags a corpus declares for itself, through `[schema] overlay`, and
      `undefined-tag` for the ones nobody declared
- [ ] No prose rules. `markdoc-prose` is a separate crate, on harper-core
- [ ] `[rules.…]` is per rule, not per id: `[rules.syntax]` re-levels
      `unclosed-tag` and `malformed-anchor` together, and `unparsable`
      belongs to no rule, so nothing switches it off. `[corpus] skip` is per
      file rather than per rule
- [ ] The CLI cannot list the rule set. `rules::status` knows what is on,
      off or re-levelled and what would turn a silent rule on, but no
      `markdoc-lint` flag prints it
- [ ] `from_frontmatter` collects every page's frontmatter keys into one set,
      so a key defined in one page counts as defined everywhere. It is a
      list of plausible names, not a per-page scope — and it collects
      nothing unless `anchors.declare_tag` is set, because the keys are
      gathered in the pass that collects anchors
- [ ] `duplicate-anchor` is per file. Two files in the same scope declaring
      the same id is not reported at all: the first in path order wins, and
      every reference lands there — consistently, and with no way to find out
- [ ] A path given on the command line that is outside `--root`, or that
      does not exist, is skipped without a word, where it should say so
- [ ] A `[[layout]]` without a `require` is accepted and checks nothing,
      where it should be refused the way an unknown key is
- [ ] The index is rebuilt from scratch on every run. Fine at a thousand
      files; nothing is cached between runs
- [ ] A tag left open inside a fenced block takes the rest of the page into
      the fence, as upstream parses it, and the convention rules do not
      look inside fences: until the tag is closed they see nothing after
      it. `syntax` reports the tag, so that is the first thing fixed

## Licence

MIT. See [LICENSE](LICENSE).

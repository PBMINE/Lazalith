# Lazen Formatting

This document is Step 90 of the roadmap. There is **one** style, `lazen fmt`
produces it, and this is what it says.

## The one thing the formatter is allowed to do

Rewrite whitespace. Nothing else.

That is not a limitation being apologised for. It is the property that makes a
formatter safe to run on someone else's file, and the tests assert it directly: the
formatted text lexes to the same token stream as the input, with the same comments, in
the same order. A formatter that changed a token would be a compiler, and this
repository has one.

Working from the token stream rather than from the source text is what makes that
cheap to guarantee. Literals are tokens, so there is no "did I mean to rewrite this
string" question to answer. A formatter built on regular expressions over source text
has to answer that question for every construct in the language, forever; one built on
the lexer answers it once, by construction.

## The style

### Indent

Four spaces per level. Never a tab.

A tab is a request for a width the reader's editor chooses. This repository already
forbids `unsafe`, and this is the same instinct applied to files: the reader's
environment should not be able to change what the file means, or how long its lines
are.

The depth is capped. A pathologically nested file gets a deep indent rather than a
line of kilobytes of spaces.

### Spacing

| | |
| --- | --- |
| `f(x)`, `xs[i]`, `T(x)` | tight — a call, a subscript, a type's brackets |
| `if (a)` | a space — a keyword's parenthesis is punctuation, not a call |
| `a::b::c`, `p.x`, `a..b` | tight — a path is a path |
| `a + b`, `a == b`, `a && b` | one space around a binary operator |
| `x: i32` | nothing before the `:`, one space after |
| `f(a, b, c)` | nothing before a `,`, one space after |
| `a-> i32` | one space around `->` |
| `fn f() -> i32 {` | one space before the brace |

### Statements

One per line, ended by `;`. A `;` inside an array's length (`[u8; 16]`) or a record's
field list (`point { x: 1, y: 2; }`) is a separator, not an ending, and the formatter
knows the difference by counting the brackets it has open.

### Blocks

A `{` opens a block, and its contents go on their own lines, indented one level. A `}`
returns to the level the block was opened at.

Three things the author wrote are left alone, because each one is a claim about the
program that a formatter should not overrule:

- `struct S {}` stays `struct S {}`. An empty body means "nothing here", and a brace
  on its own line is a claim about the body the author did not make.
- `if a { b(); }` stays on one line. Same reason.
- `) {` stays `) {`. It is how a multi-line call ends and its block begins, and
  separating them splits one construct into two halves.

### Blank lines

A blank line the author wrote is **kept**, one of them. A blank line is grouping, and
a formatter that deletes every blank line in a file deletes its structure — which is
the difference between a formatter and a refactoring.

Runs collapse: three blank lines become one, so the output cannot grow on each pass.
A blank line before a comment is kept, because that is where grouping usually happens
in a file that explains itself.

### Line breaks inside a call

A call whose arguments the author wrote one per line stays that way.

A token stream has no width and no notion of where a line was, so *keeping the
author's choice* is the only thing a formatter built on tokens can do here — and it
is also the only thing worth doing, because the alternative is a formatter that joins
a 40-line call into one unreadable 400-character line. The arguments are indented one
level deeper than the line the call starts on.

### Comments

Kept, in order, at the current indent. A comment's *text* keeps its own leading
whitespace, because a space after the `//` is content:

```lazen
//     lazen run main.lz
```

means it. The indent *before* the `//` is not part of the text, so re-indenting the
`//` cannot disturb what is inside it.

A comment after the last token in a file stays. A note at the end of a file is a note
to whoever reads it next, and dropping it because it came after the last semicolon
would be the formatter eating a person's writing.

### The end of the file

Exactly one trailing newline. No trailing whitespace on any line. A file with nothing
in it stays empty rather than becoming a newline.

## The one thing tokens cannot say

`-` is subtraction in `a - b` and negation in `return -x;`, and the two are the same
token. A formatter built on tokens alone has three bad options: guess (and produce
`a - x` for a negation, or `a-b` for a subtraction), refuse to format anything with a
sign in it, or parse the file — which would mean the formatter only works on a file
that already compiles, at exactly the moment a person most wants to format one.

So the formatter reads the source for this one decision and preserves what the author
wrote:

```text
no space before, space after   -> binary     a - b
space before, no space after   -> unary      return -x;
```

When the author was not clear — both spaces, or neither — the conservative reading
wins: a token after something that can end an expression is infix, and a token after
a keyword is a prefix. The lexer has keyword tokens of its own, so "after a keyword"
is a fact about the token rather than a guess about the text.

This is also why the style is idempotent: the output's own text disambiguates the
next run exactly the same way.

## Idempotence is the property that makes this usable

`fmt(fmt(x)) == fmt(x)`. Always.

A formatter that is not idempotent fights the next `fmt`, and a repository that says
"run `lazen fmt`" with no idempotence test is asking people to check `git diff` every
time for no reason.

Every rule above is either forced by the tokens or a *preservation* of something the
author wrote, and a preservation is stable by construction: the output re-lexes to
text with the same line breaks and the same disambiguating spaces, so the second pass
reaches the same answer. `crates/lazalith-compiler/tests/format.rs` checks it for
every style fixture and for `examples/window/main.lz`, which is the largest program in
the repository — and which is already written this way, so the formatter leaves it
byte for byte alone. The repository's own code is the style's worked example.

## The command

```text
lazen fmt [--check] [file]
```

`fmt` rewrites the file. `fmt --check` changes nothing, exits 1 if a file would change,
and prints the first few lines that differ.

They are separate because the first rewrites a person's file and a script must not do
that by accident. A file that is already formatted is reported and exits 0 either way,
so a script can run this unconditionally.

A file that does not lex is **refused**, with the lexer's reason, and is not touched.
A file that does not compile but does lex is formatted happily — which is the point,
since the usual moment to run a formatter is on a file that does not yet compile.

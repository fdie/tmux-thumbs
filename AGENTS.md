# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## What this is

`tmux-thumbs` is a Rust rewrite of tmux-fingers: it scans the visible tmux pane for
patterns (paths, URLs, SHAs, IPs, UUIDs, hex colors, etc.), overlays keyboard hints
(vimium/vimperator-style) on each match, and copies the selected match into the tmux
buffer (or a user-defined command). It ships as both a tmux plugin and a standalone
CLI (`thumbs`) that can be piped into like `fzf`.

## Common commands

```sh
cargo build                      # debug build
cargo build --release            # release build (used by tmux-thumbs.sh / swapper)
cargo fmt --all -- --check       # CI runs this; use `cargo fmt --all` to fix
cargo test                       # run all unit tests
cargo test match_urls            # run a single test by name (works across files)
cargo test --bin thumbs          # only src/main.rs + src/state.rs + src/view.rs tests
cargo test --bin tmux-thumbs     # only src/swapper.rs tests
```

There is no separate lint step beyond `cargo fmt --check` in CI; formatting uses
`.rustfmt.toml` (`tab_spaces = 2`, `max_width = 120` — note 2-space indent, not
the Rust default of 4).

To manually exercise the picker outside of tmux:

```sh
cargo run --bin thumbs < samples/test1
```

`samples/` contains fixture text (ANSI-colored output, lorem ipsum with embedded
IPs/paths/etc.) useful for manual testing of pattern matching and rendering.

## Architecture

Two independent binaries share the `regex`/pattern-matching core but have separate
responsibilities:

### 1. `thumbs` (src/main.rs, src/state.rs, src/view.rs, src/alphabets.rs, src/colors.rs)

The actual picker UI. Reads captured pane text from stdin, runs it through
`state.rs`, and renders an interactive terminal overlay via `view.rs`.

- **`state.rs`**: Defines `PATTERNS` (an ordered array of `(name, regex)` tuples —
  order determines match priority when multiple patterns could match the same
  text) and `EXCLUDE_PATTERNS` (patterns recognized but never hinted, e.g. bash
  color escape codes, so they don't corrupt offsets). `State::matches()` walks
  each line, repeatedly finds the earliest/highest-priority match in the
  remaining chunk, extracts capture groups (a named `match` group takes
  precedence, otherwise all non-group-0 captures are used, e.g. for
  `diff --git a/X b/Y` yielding two matches), then assigns hint strings from
  `alphabets::get_alphabet()`. Supports `reverse` (shorter hints near the
  cursor) and `unique` (same text always gets the same hint) modes via careful
  double-reversal of the `matches`/`hints` vectors — read the comments there
  before changing this logic, it's easy to get backwards.
- **`alphabets.rs`**: Maps alphabet names (qwerty, dvorak, colemak, homerow
  variants, etc.) to letter strings and expands them into multi-character hints
  (`"da"`, `"db"`, ...) when there are more matches than single letters.
- **`view.rs`**: Owns the `termion` raw-mode/alternate-screen rendering loop.
  `render()` draws the captured lines, highlights each match, and overlays hint
  text at a **display-column** offset — `mat.x` from `state.rs` is a *byte*
  offset, so it's converted via `display_width()`, which expands literal tabs
  to the next tab stop (tmux's `capture-pane` preserves tabs verbatim) and
  accounts for wide/CJK characters. `listen()` is the key-handling loop: normal
  mode exits on first full hint match, multi mode (toggled by Space, or
  `--multi`) accumulates selections until Space is pressed again, arrow keys
  move the "selected" match without needing to type its hint, and an uppercase
  final letter marks a selection for auto-paste.
- **`colors.rs`**: Parses named colors or `#RRGGBB` hex into `termion::color`
  instances for all the `--*-color` options.

### 2. `tmux-thumbs` (src/swapper.rs)

The tmux integration glue, run by `tmux-thumbs.sh`. It never touches the pattern
matching — its job is orchestrating tmux itself:

1. `capture_active_pane()` — parses `tmux list-panes` to find the active pane's
   id, height, scroll position (if in copy-mode), and zoom state.
2. `execute_thumbs()` — reads `tmux show -g` for all `@thumbs-*` options,
   translates them into `thumbs` CLI flags via regex, opens a new hidden tmux
   window running `tmux capture-pane | thumbs -t /tmp/thumbs-last`, and signals
   completion with `tmux wait-for`.
3. `swap_panes()` / `resize_pane()` — swaps the hidden `thumbs` pane into the
   active pane's place (and re-zooms if needed) so the picker overlay appears
   exactly where the original content was.
4. `wait_thumbs()` / `retrieve_content()` / `destroy_content()` — blocks until
   `thumbs` signals it's done, reads the result from the tmp file, deletes it.
5. `execute_command()` — runs the user's configured command
   (`@thumbs-command` / `-upcase-command` / `-multi-command`), substituting the
   picked text. Selected text is passed through an intermediate shell variable
   (`THUMB="$1"; eval "$2"`) rather than spliced directly into the command
   string — see the long comment above `execute_final_command` — to avoid
   shell-injection-by-match-content (e.g. a matched string like `foo;rm *`).

All tmux interaction goes through an `Executor` trait (`RealShell` in production,
a scripted `TestShell` in tests), which is what makes `swapper.rs`'s tests able to
assert on exact tmux command invocations without a real tmux session.

### Shell entry points

- `tmux-thumbs.tmux` — TPM plugin entry point; binds `@thumbs-key` (default
  Space) to a `thumbs-pick` command alias.
- `tmux-thumbs.sh` — invoked by the key binding; checks the release binaries
  exist and match `Cargo.toml`'s version (triggering
  `tmux-thumbs-install.sh` in a split pane if not/outdated), then reads
  `@thumbs-*` tmux options and execs the `tmux-thumbs` binary.
- `tmux-thumbs-install.sh` — builds the release binaries with cargo (invoked
  automatically on first run or version mismatch).

## Testing conventions

Tests live inline in `#[cfg(test)] mod tests` blocks at the bottom of each
source file (not in a separate `tests/` dir). `state.rs` tests are the largest
and most important surface for pattern-matching changes — when adding or
changing a regex in `PATTERNS`, add a corresponding `match_*` test there, and
check the `priority` test, which asserts cross-pattern ordering on a line
containing many pattern types at once.

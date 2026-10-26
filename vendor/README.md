# Patched gpui-ce crates

Copies of two [gpui-ce](https://crates.io/crates/gpui-ce) 0.3.0 crates, used through
`[patch.crates-io]` in the workspace `Cargo.toml`. They are licensed under Apache-2.0
(`LICENSE-APACHE` in each folder), not under backfill's MIT licence. Every change is marked
with a "Patched for backfill" comment.

- `gpui_ce_macos`: `src/platform.rs` releases the platform lock before the launch callback,
  which otherwise deadlocks when it opens a window.
- `gpui_ce_elements`: `src/editable_text/state.rs` and `element.rs` scroll a single-line
  field to its caret after each edit. The examples and tests were left out of the copy, and
  out of `Cargo.toml`.

Drop each patch once a gpui-ce release includes the fix.

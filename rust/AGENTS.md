# AGENTS.md

## Scope

These instructions apply to the Rust workspace rooted at this directory.

## Working Rules

- Keep Rust output byte-compatible with the Java oracle unless the active task explicitly defines a narrower parity gate.
- Prefer existing crate boundaries: `earthmap-core`, `earthmap-minecraft`, `earthmap-region`, `earthmap-parity`, and `earthmap-cli`.
- Keep unsafe code out unless a future task explicitly approves and documents it.
- Do not revert or overwrite unrelated dirty worktree changes.
- Stage files explicitly. Never rely on broad staging commands when unrelated changes are present.
- Use `EARTHMAP_HEIGHTMAP`, `EARTHMAP_DATA_ROOT`, `EARTHMAP_TIF_ROOT`, and `EARTHMAP_OUTPUT_ROOT`
  for local data paths in Rust CLI smoke runs, parity probes, and documentation examples. The Rust CLI
  accepts omitted heightmap arguments only when `EARTHMAP_HEIGHTMAP` is set.

## Verification

- Run `cargo fmt --all --manifest-path rust/Cargo.toml` after Rust edits from the repository root.
- Run targeted `cargo test --manifest-path rust/Cargo.toml -p <crate> --locked` for scoped changes.
- Run `cargo test --manifest-path rust/Cargo.toml --workspace --locked` before completing changes that affect shared crates, CLI behavior, or parity fixtures.
- For Java/Rust parity work, run the relevant script under `rust/scripts/` and record the output path or summary in the task notes.

## Commit Discipline

- Every task must end with an atomic Conventional Commit when the change is ready.
- Use Conventional Commits format, for example `docs: add rust agent workflow instructions` or `fix: match java region writer bytes`.
- Keep one logical change per commit.
- Do not include unrelated user or generated changes in the commit.
- If a safe commit is impossible because unrelated edits overlap the same files, stop and explain the conflict before committing.

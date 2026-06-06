# AGENTS.md

## Scope

These instructions apply to the Rust-only EarthMap project rooted at this directory.

## Working Rules

- Read the relevant code and tests before editing.
- Preserve Minecraft, Linear, MCA, visual-quality, and metric compatibility unless the task explicitly changes behavior.
- Do not use byte-for-byte Java parity as a default completion condition. File-format invariants still matter.
- Do not revert or overwrite unrelated dirty worktree changes.
- Stage files explicitly. Never rely on broad staging commands when unrelated changes are present.
- Use `EARTHMAP_HEIGHTMAP`, `EARTHMAP_DATA_ROOT`, `EARTHMAP_TIF_ROOT`, and `EARTHMAP_OUTPUT_ROOT`
  for local data paths in CLI examples, parity probes, and verification runs unless a test fixture or
  user request explicitly names another file.

## Verification

- Prefer targeted tests first, then broader tests when the change affects shared behavior.
- Use `.\scripts\test.ps1 -Filter '<TestNameOrPattern>'` for focused Rust regression checks.
- Use `cargo test --manifest-path rust\Cargo.toml --workspace` for full workspace verification.
- Run the full relevant suite before marking high-risk generation, region, NBT, or CLI changes complete.

## Commit Discipline

- Every task must end with a conventional atomic commit when the change is ready.
- Use Conventional Commits format, for example `docs: add agent workflow instructions` or `fix: preserve region payload parity`.
- Keep one logical change per commit.
- Do not include unrelated user or generated changes in the commit.
- If a safe commit is impossible because unrelated edits overlap the same files, stop and explain the conflict before committing.

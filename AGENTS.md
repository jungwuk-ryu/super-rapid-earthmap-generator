# AGENTS.md

## Scope

These instructions apply to the Java project rooted at this directory.

## Working Rules

- Read the relevant code and tests before editing.
- Preserve Java output compatibility unless the task explicitly changes behavior.
- Do not revert or overwrite unrelated dirty worktree changes.
- Stage files explicitly. Never rely on broad staging commands when unrelated changes are present.

## Verification

- Prefer targeted tests first, then broader tests when the change affects shared behavior.
- Use `.\scripts\test.ps1 -Filter '<TestClassOrPattern>'` for focused Java regression checks.
- Run the full relevant suite before marking high-risk generation, region, NBT, or CLI changes complete.

## Commit Discipline

- Every task must end with a conventional atomic commit when the change is ready.
- Use Conventional Commits format, for example `docs: add agent workflow instructions` or `fix: preserve region payload parity`.
- Keep one logical change per commit.
- Do not include unrelated user or generated changes in the commit.
- If a safe commit is impossible because unrelated edits overlap the same files, stop and explain the conflict before committing.

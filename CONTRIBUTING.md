# Contributing

Thanks for helping improve Super-Rapid EarthMap Generator.

## Project Layout

- Active Rust workspace: `rust/`
- Root wrappers: `scripts/build.ps1`, `scripts/test.ps1`, `scripts/run.ps1`
- Operator docs: `docs/`

## Development Rules

- Keep generated worlds, rasters, logs, and local data outside Git.
- Keep changes focused and commit as atomic Conventional Commits.
- Preserve Minecraft region format compatibility and existing quality gates unless a change explicitly updates them.
- Prefer adding focused tests for behavior changes.

## Local Verification

```powershell
.\scripts\build.ps1
cargo fmt --all --manifest-path rust\Cargo.toml -- --check
cargo clippy --manifest-path rust\Cargo.toml --workspace --all-targets -- -D warnings
.\scripts\lint.ps1
.\scripts\test.ps1
```

Use `.\scripts\test.ps1 -Filter <pattern>` for focused checks and `.\scripts\test.ps1 -Isolated` for tests that need
one-at-a-time debugging.

## Pull Requests

Describe the user-visible behavior, the risk level, and the verification commands you ran. Include screenshots or
generated-world evidence when changing GUI behavior or terrain output.

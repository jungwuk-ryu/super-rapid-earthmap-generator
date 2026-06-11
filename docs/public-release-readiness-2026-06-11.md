# Public GitHub Release Readiness Review - 2026-06-11

## Purpose

Assess whether `super-rapid-earthmap-generator` is ready to be published as a public GitHub repository.

Important context:

- The project used to be Java-based.
- The active implementation is now Rust, but the Rust workspace lives under `rust/`.
- Public readers may land at the repository root and miss the active Rust code unless the root docs and scripts make that explicit.

## Review Strategy

- Parent reviewer: repository-level release readiness, GitHub hygiene, security/secrets, licensing, and final decision.
- Sub-agent A: Rust implementation, workspace, build/test/CI, and release engineering readiness.
- Sub-agent B: public-facing documentation, onboarding, discoverability, migration messaging, and contributor experience.
- Method: list every topic before review, inspect each topic, then mark its checkbox only after evidence is recorded.
- Checkbox meaning: checked means "reviewed", not "passed". Pass/fail is recorded in the evidence log and final decision.

## Progress

- Status: complete
- Checklist: 44 / 44 reviewed
- Final readiness decision: NO-GO for public open-source publication as-is
- Last updated: 2026-06-11 KST

## Checklist

### Repository Identity And Scope

- [x] T01 - Repository name, purpose, and positioning are clear to first-time visitors.
- [x] T02 - Root `README.md` clearly states that the active codebase is Rust under `rust/`.
- [x] T03 - Old Java-era directories/files are either removed, archived, ignored, or explained.
- [x] T04 - Root-level scripts route users to the Rust implementation without ambiguity.
- [x] T05 - Repository does not expose local IDE, agent, build, cache, or personal workspace artifacts.

### GitHub Public Hygiene

- [x] T06 - `.gitignore` covers Rust build output, generated worlds, logs, local data, IDE files, and agent outputs.
- [x] T07 - GitHub Actions workflows are present and suitable for public CI.
- [x] T08 - Issue/PR templates, contribution guidance, and support expectations are present or consciously deferred.
- [x] T09 - Repository metadata gaps are identified: description, topics, homepage, license badge, CI badge, and release badge.
- [x] T10 - Default branch history and current worktree are clean enough for publication.

### Licensing, Attribution, And Data Rights

- [x] T11 - A root `LICENSE` file exists and matches intended open-source terms.
- [x] T12 - Third-party dependency licenses are reviewable and compatible enough for publication.
- [x] T13 - Bundled assets, images, CSVs, pins, and example configs have source/rights clarity.
- [x] T14 - Minecraft/Mojang/server references avoid implying affiliation or redistributing restricted artifacts.
- [x] T15 - Generated artifact policy makes clear what should not be committed.

### Security And Privacy

- [x] T16 - No obvious secrets, tokens, hostnames, passwords, or personal paths are committed.
- [x] T17 - Scripts avoid dangerous defaults for public users.
- [x] T18 - Network/download behavior is documented and pinned where needed.
- [x] T19 - Untrusted input handling risks are noted for geospatial rasters, JSON, properties, and output paths.
- [x] T20 - Supply-chain risks are visible: Cargo lockfile, CI pinning, and dependency audit path.

### Rust Workspace And Build System

- [x] T21 - `rust/Cargo.toml` workspace layout is coherent and discoverable from the root.
- [x] T22 - Root build/test/lint scripts work from the repository root.
- [x] T23 - `cargo fmt`, `cargo clippy`, and tests have a documented expected command path.
- [x] T24 - Release profile, binary names, and install/run commands are public-user friendly.
- [x] T25 - Java-to-Rust migration leaves no stale runtime references that mislead users.

### Test, Quality, And CI Readiness

- [x] T26 - Test suites pass locally in the current repository state.
- [x] T27 - CI covers formatting, linting, tests, and the root wrapper scripts.
- [x] T28 - Quality gates for generated Minecraft worlds are documented and reproducible enough.
- [x] T29 - Expensive/manual tests are separated from normal CI and documented.
- [x] T30 - Known ignored tests, missing fixtures, or environmental assumptions are visible.

### CLI, GUI, UX, And Public Onboarding

- [x] T31 - CLI help and error messages make the first successful run achievable.
- [x] T32 - GUI launch instructions are clear and do not depend on hidden local paths.
- [x] T33 - Sample configs and commands use portable placeholders rather than personal paths.
- [x] T34 - Progress, resume, and failure behavior are documented for long-running generation.
- [x] T35 - Windows PowerShell usage is clear, since local scripts target Windows heavily.

### Performance, Scale, And Operational Readiness

- [x] T36 - Performance expectations and hardware/disk requirements are explicit.
- [x] T37 - Large-output behavior, cleanup, resume, and disk safety are documented.
- [x] T38 - Benchmarks or performance TODOs are current and not misleading.
- [x] T39 - Concurrency defaults and worker/shard behavior are documented for public operators.
- [x] T40 - Failure recovery and troubleshooting docs are sufficient for non-authors.

### Documentation Architecture And Maintenance

- [x] T41 - `docs/README.md` is a reliable documentation index.
- [x] T42 - Architecture/module documents match the current Rust crate layout.
- [x] T43 - Completed, obsolete, or Java-era docs have been removed or archived deliberately.
- [x] T44 - Public release blockers and follow-up recommendations are summarized with severity.

## Evidence Log

### Parent Review - Repository, Security, Licensing, And CI

- T01: Root README describes the project as a photo-first Minecraft 1.21.11 Earth surface generator, but also says the current state is `active prototype, production readiness NO-GO`.
- T02: Root README build commands use `rust/Cargo.toml`, but it does not explicitly say "the active codebase is Rust under `rust/`"; this is a public onboarding gap.
- T03/T05: `git ls-files` does not include local `.idea/`, `agent-runs/`, `src/`, or `NaturalSurfaceBlockPolicyTest/`; `git status --ignored --short` confirms those are ignored. They still exist in the working directory and should be removed before creating/uploading a public archive.
- T04/T22: `scripts/build.ps1`, `scripts/test.ps1`, and `scripts/run.ps1` delegate to `rust/scripts/*` and print Rust wrapper markers.
- T06/T15: `.gitignore` covers `build/`, `.idea/`, `rust/target/`, `rust/target-latest/`, logs, `worlds/`, `quality/`, previews, agent runs, and server logs. It does not ignore `/out/`, even though docs recommend `out/` artifact roots.
- T07/T27: `.github/workflows/ci.yml` exists and runs fmt, clippy, repository lint, and tests on Windows. Risk: push trigger only names `main`, while the current local branch is `master`.
- T08/T09: No `CONTRIBUTING.md`, `SECURITY.md`, `SUPPORT.md`, code of conduct, issue templates, PR template, README badges, or repository metadata guidance were found.
- T10: Worktree became dirty only because this review document was added. Before this review, the tree was clean after the previous commits.
- T11: No root `LICENSE` or `LICENSE.md`; `rust/Cargo.toml` declares `license = "UNLICENSED"`. This is a release blocker for open-source publication.
- T12/T20: `Cargo.lock` is committed, but `cargo audit` and `cargo deny` are not installed/configured, and no dependency-license/advisory workflow is present.
- T13: Bundled GUI asset `rust/crates/earthmap-gui/assets/world-background-truemarble-2048.png`, CSV research files, and archived pin JSONs lack an explicit attribution/rights inventory.
- T14: No server jars are committed, and `vendor/minecraft/*/server.jar` is ignored. A public non-affiliation/distribution disclaimer for Minecraft/Mojang/DivineMC is still missing.
- T16: High-specificity token/private-key scan found no obvious API keys. Broader scan found local absolute paths in docs and a hardcoded local RCON default password in scripts.
- T17: `scripts/run-server-finalization-gate.ps1`, `scripts/run-server-finalization-windows.ps1`, and `scripts/run-nation-war-acceptance-gate.ps1` default `RconPassword` to `earthmap-codex-rcon`; this is not a secret, but it is unsafe as a public default.
- T18: Archived pins exist for Minecraft/DivineMC metadata, but network/download behavior and refresh policy are not collected in one public-facing place.
- T19: The Rust code has many validation tests for rasters, region bounds, and resume payloads, but public docs do not summarize the untrusted-input/security model.
- T21: `rust/Cargo.toml` is a coherent workspace with 11 crates and `resolver = "2"`.
- T23/T26: Current verification passed: `cargo fmt --all --manifest-path rust\Cargo.toml -- --check`; `cargo clippy --manifest-path rust\Cargo.toml --workspace --all-targets -- -D warnings`; `.\scripts\lint.ps1`; `.\scripts\test.ps1` with 127 CLI tests passed, 2 ignored, plus crate tests passed.
- T24/T31: `earthmap-rs --help` and GUI `--cli --version` work, but the CLI help is long and diagnostic-heavy; there is no beginner-safe "first successful generation" path in the root README.
- T25: `.\scripts\check-no-earthmap-java-runtime-refs.ps1` passes. Remaining Java references are mostly compatibility/oracle language, but `build.config.json` still contains `javaLanguageVersion`.
- T28/T29/T30: Quality gates are documented as NO-GO for full world generation; slow full-region tests are explicitly ignored in test output.
- T32/T35: GUI and PowerShell usage are documented in `docs/OPERATIONS.md`.
- T33: Current setup examples use placeholders, but `docs/EXPERIMENTS.md`, `docs/PERFORMANCE-OPTIMIZATION-TODO.md`, and archived pins contain many local `D:\earthmap\...` and `C:\...` paths.
- T34: Progress/resume behavior is partly covered through CLI options and operations docs, but troubleshooting is thin for public users.
- T41/T42/T43: `docs/README.md` exists and indexes active docs; architecture/module docs point to Rust crate boundaries. Some old Java-compatible terminology remains by design, but it needs public framing.
- T36/T37/T38/T39/T40: `docs/PERFORMANCE-OPTIMIZATION-TODO.md` is current and detailed, but it is an engineering work log rather than a public operator guide. It still records open crash/premature-termination work, performance tuning uncertainty, and many local machine paths. `docs/OPERATIONS.md` has only short recovery guidance and does not give public sizing limits for GUI sharding, worker counts, disk space, or expected duration.

### Sub-Agent A - Rust Workspace, Build, Runtime, And Release Engineering

- Blocker T24/T11: workspace declares `version = "0.0.0-phase0"` and `license = "UNLICENSED"` with no root `LICENSE`.
- Blocker T28/T36/T40: root README and quality gates explicitly say production/full-world generation is NO-GO; public release must not be positioned as production-ready.
- Blocker T36/T38/T40: performance notes retain unresolved `0xC0000005` / premature-termination risk for long-running generation.
- High T20/T23/T27: CI and wrapper scripts do not enforce `--locked`; root CI calls wrapper scripts that run cargo without lockfile enforcement.
- High T25: `build.config.json` still advertises `javaLanguageVersion = 25`; Java-reference scanner exists but is not wired into CI or lint.
- High T29/T30: release-relevant slow/full-region tests are ignored in normal CI and need documented manual release gates.
- Medium T36/T39: GUI permits high concurrency and shard counts; docs warn qualitatively but do not give public sizing guidance.
- Medium T37/T40: recovery docs are too thin for interrupted public generation runs.
- Positive: root wrappers, fmt, clippy, tests, CLI help/version, and GUI CLI-version path worked.

### Sub-Agent B - Documentation, Onboarding, And Trust Surface

- Blocker T11: no root license and workspace is `UNLICENSED`.
- High T02/T03/T25/T43: root README does not plainly state active Rust-under-`rust/`; local Java-era root directories would confuse first-time users if included in release packaging.
- High T08: contribution, support, security, issue, and PR guidance are absent.
- High T13: bundled GUI image and required TrueMarble data path lack attribution/asset license notes.
- Medium T01/T31: public status is honest, but onboarding jumps into internal batch commands.
- Medium T05/T06/T15/T37: generated artifact policy exists, but `.gitignore` omits `/out/`.
- Medium T09/T10: repository badges/metadata guidance absent; current review doc is untracked until committed.
- Medium T12/T14: no dependency-license review path; no public non-affiliation disclaimer.
- Medium T33/T38: local absolute paths remain in docs.
- Medium T34/T36/T39/T40: public operator troubleshooting and hardware/disk expectations are scattered.

## Sub-Agent Findings

Sub-agent A completed; findings integrated above.

Sub-agent B completed; findings integrated above.

## Final Decision

NO-GO.

The repository is technically buildable and testable, but it is not ready to publish as a public open-source GitHub repository in its current state.

## Severity Summary

### Blockers

1. Licensing is unresolved: no root `LICENSE`, workspace `license = "UNLICENSED"`.
2. Project is explicitly not production-ready: README and quality gates say full generation is NO-GO.
3. Long-run generation has unresolved native crash/premature-termination evidence.
4. Public asset/data rights are not inventoried, including bundled GUI imagery and required TrueMarble-style data assumptions.

### High

1. Root README does not explicitly frame the active Rust workspace under `rust/`.
2. Public contribution/security/support expectations are missing.
3. CI does not enforce Cargo lockfile use through the root wrappers.
4. Java-era signals remain: `build.config.json` still says `javaLanguageVersion`, and local legacy directories exist in the working copy.
5. Release-relevant ignored/manual tests are not documented as a public release gate.

### Medium

1. `.gitignore` omits `/out/` while docs recommend `out/` for scratch artifacts.
2. Dependency license/advisory audit tooling is not configured.
3. Public operator docs lack hardware/disk/time sizing and safe concurrency guidance.
4. Troubleshooting/recovery docs are too thin for non-authors.
5. Docs retain many local absolute paths in experiment/performance history.

## Minimum Before Publishing

1. Choose and add the intended license, or document all-rights-reserved publication intentionally.
2. Add root README onboarding that says the active Rust workspace is under `rust/`, with one beginner-safe successful run.
3. Add `CONTRIBUTING.md`, `SECURITY.md`, support expectations, and GitHub issue/PR templates or explicitly defer them in README.
4. Add asset/data attribution and Minecraft/Mojang/DivineMC non-affiliation/distribution disclaimers.
5. Align CI branch trigger with the actual default branch and make wrappers/CI use `--locked`.
6. Add `/out/` to `.gitignore` or stop recommending it as a repo-local scratch root.
7. Remove or archive local legacy Java-era directories before packaging/publishing.
8. Close or clearly label the long-run crash/premature-termination risk.
9. Document manual release gates for ignored full-region/quality tests.
10. Publish only as an active prototype unless the quality gates are promoted.

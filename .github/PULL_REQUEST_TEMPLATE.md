## Summary

- Describe the user-visible change and the implementation scope.

## Verification

- [ ] `.\scripts\build.ps1`
- [ ] `cargo fmt --all --manifest-path rust\Cargo.toml -- --check`
- [ ] `cargo clippy --manifest-path rust\Cargo.toml --workspace --all-targets -- -D warnings`
- [ ] `.\scripts\lint.ps1`
- [ ] `.\scripts\test.ps1`

## Risk

- [ ] CLI behavior
- [ ] GUI behavior
- [ ] Generated world output
- [ ] Documentation only
- [ ] External data or artifact handling

## Notes

- Add migration, release, or follow-up notes here.

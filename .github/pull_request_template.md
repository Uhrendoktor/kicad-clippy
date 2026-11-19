## Summary

<!-- Describe the behavior or problem addressed. -->

## Scope

- [ ] Parser / source mapping
- [ ] Native KiCad ERC adapter
- [ ] Custom rule engine
- [ ] Rule/profile configuration
- [ ] Diagnostics / output
- [ ] Fixes / layout
- [ ] Documentation / tooling

## Validation

- [ ] `cargo fmt --all -- --check`
- [ ] `cargo clippy --workspace --all-targets --all-features --locked -- -D warnings`
- [ ] `cargo test --workspace --all-features --locked`
- [ ] `cargo test --workspace --doc --locked`
- [ ] Native KiCad ERC smoke test (when relevant)

## Documentation

- [ ] README and relevant docs are updated.
- [ ] Implemented behavior is distinguished from planned behavior.

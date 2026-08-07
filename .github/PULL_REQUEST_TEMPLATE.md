## What changed

## Why

What problem this solves. If there's a design decision in here that isn't obvious from the
code, say why you went this way and what you didn't do instead.

## How you tested it

New tests are the expectation for anything but pure docs/config changes. If you couldn't test
something (e.g. it needs a live dbt Fusion install), say that plainly.

## Checklist

```bash
cargo fmt --all -- --check
cargo clippy --all-targets --all-features -- -D warnings
cargo test --all-features
```

- [ ] `cargo fmt --all -- --check` passes
- [ ] `cargo clippy --all-targets --all-features -- -D warnings` passes
- [ ] `cargo test --all-features` passes
- [ ] New/changed behavior has test coverage (or this PR is docs/config-only)
- [ ] I've read and signed the [CLA](CLA.md) (see [CONTRIBUTING.md](CONTRIBUTING.md))

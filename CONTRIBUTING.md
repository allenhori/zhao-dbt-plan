# Contributing

Thanks for considering a contribution to `zhao-dbt-plan`.

## Contributor License Agreement (CLA)

This project is licensed [AGPLv3](LICENSE), and the maintainer also offers a separate
commercial license to organizations that don't want AGPLv3's obligations. To keep that option
open, every external contribution needs a signed CLA before it can be merged — an **individual
CLA** (you keep your own copyright; you grant the maintainer a license broad enough to also
offer your contribution under a different license later), not a full copyright assignment.

The first time you open a pull request, a bot will comment asking you to sign — just reply on
that comment as instructed. This is a one-time step; it's remembered for all your future PRs.

## Development

```bash
cargo fmt --all -- --check
cargo clippy --all-targets --all-features -- -D warnings
cargo test --workspace
```

Some tests (`tests/end_to_end.rs`) need a real `dbt` install to run — they skip automatically
(not fail) if `dbt` isn't on `PATH`. Set `ZHAO_DBT_PLAN_TEST_DBT_COMMAND` to point at a specific
`dbt` binary (e.g. a venv's), and `ZHAO_DBT_PLAN_TEST_FUSION_COMMAND` at a dbt Fusion binary to
additionally run the cross-engine compatibility check.

## Pull requests

- New tests are expected for anything but pure docs/config changes.
- Run the checklist above before opening, not after CI tells you.
- Say what you tested and how in the PR description — especially anything that couldn't be
  covered by an automated test (e.g. needs a live warehouse).

## Scope

This addon is deliberately **plan-only, permanently** — it never executes `dbt build`/`dbt
run`, and never will. PRs adding execution behavior won't be accepted; open an issue on this
repo to discuss scope questions first, before investing time in a larger change.

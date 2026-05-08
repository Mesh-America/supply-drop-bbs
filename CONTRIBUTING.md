# Contributing to Supply Drop BBS

Thanks for the interest. This document covers what you need to know
before sending a contribution.

## License grant on contributions

By submitting a contribution to this project — whether a pull request,
patch, code in an issue, design proposal in a comment, or any other form
of authored material directed at incorporation — **you agree that your
contribution is licensed to the project under the same license as the
project itself** (Apache 2.0 with Commons Clause).

You **also grant the project's licensor (currently Taedryn / TJ Downes) a
perpetual, worldwide, non-exclusive, royalty-free license to relicense
your contribution under different terms** in any future version of the
project. This is the relicensing-flexibility insurance the project keeps
in case the license needs to change later (e.g. to drop the Commons
Clause, to switch to a fully OSI-approved license, or to adopt a future
revision of either Apache or Commons Clause).

If you cannot or will not grant either of those, please don't submit
the contribution. Open an issue describing what you'd want to share
and we can talk about other options.

## What "intentionally submitted" means

The Apache 2.0 license defines a "Contribution" as anything you
intentionally submit for inclusion. Casual mentions in chat, links to
your own work elsewhere, or text marked "Not a Contribution" are not
contributions.

If you want to share an idea without granting the rights above, mark
your message clearly: e.g. "(Not a Contribution — for discussion
only)." We'll respect that.

## Reporting bugs

Open a GitHub issue with:

- What you were doing
- What you expected
- What actually happened
- Logs (with `log_level = "DEBUG"` if you can reproduce; redact any
  passwords or session tokens before sharing)
- Your config (redacted), OS, hardware, and version

For security vulnerabilities, do not open a public issue. See
[SECURITY.md](SECURITY.md).

## Pull requests

Until the project has actual code (we're still in the
architecture-first phase), the workflow below is forward-looking.

- Branch off `main`. Branch name: `feat/<short-name>`, `fix/<short-name>`,
  or `docs/<short-name>`.
- One logical change per PR. Smaller is better.
- Commits should explain *why*, not just *what*. The diff already shows
  what changed.
- Run the full test suite locally before opening the PR.
- Add tests for behaviour changes. The project's test strategy
  (unit + integration + property + fuzz + bench + loadgen) is
  documented in [docs/ARCHITECTURE.md](docs/ARCHITECTURE.md).
- Update the relevant documentation in the same PR. Code without
  matching docs is incomplete.
- If your change affects the plugin API or the wire format, an ADR
  (architectural decision record) under `docs/adr/` is required.

## Development workflow

To be filled in once the Rust workspace is bootstrapped. Watch
`docs/OPERATIONS.md` for the development setup section.

## Code style

- Rust: `cargo fmt` (no exceptions), `cargo clippy --all-targets
  --all-features -- -D warnings` must pass.
- SQL: lowercase keywords aren't a hard rule but consistency within a
  query is. Schema migrations are append-only — never edit a migration
  that has been merged to `main`.
- Markdown: 80-char-ish lines for prose, no hard limit for tables or
  code blocks.

## Code of conduct

Read [CODE_OF_CONDUCT.md](CODE_OF_CONDUCT.md). It applies to every
interaction in this repository, including PR review.

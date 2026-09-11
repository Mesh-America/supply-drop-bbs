# Contributing to Supply Drop BBS

Thanks for the interest. This document covers what you need to know
before sending a contribution.

## License grant on contributions

By submitting a contribution to this project - whether a pull request,
patch, code in an issue, design proposal in a comment, or any other form
of authored material directed at incorporation - **you agree that your
contribution is licensed to the project under the same license as the
project itself** (Apache 2.0 with Commons Clause).

You **also grant the project's licensor (currently Mesh-America) a
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
your message clearly: e.g. "(Not a Contribution - for discussion
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
  query is. Schema migrations are append-only - never edit a migration
  that has been merged to `main`.
- Markdown: 80-char-ish lines for prose, no hard limit for tables or
  code blocks.

## CI & release workflow conventions

These apply to `.github/workflows/*.yml` and are enforced by review, not by
tooling — read this before adding or changing a workflow.

- **Don't pin a release-shipping build's glibc floor to the runner OS.**
  `release.yml`'s `build` job floats on `runs-on: ubuntu-latest` on purpose:
  every target is compiled inside a version-pinned `cross`/Docker image
  (glibc 2.23, well under any target's actual floor), so the runner image's
  own glibc never becomes the binary's minimum. Pinning `runs-on` to a fixed
  Ubuntu version instead (as `release.yml` briefly did) doesn't add safety —
  it adds an expiry date: GitHub deprecates old runner images and starts
  failing jobs pinned to them (see `actions/runner-images#14254`), which is
  exactly what forced this move. `ci.yml` and `docs.yml` can stay on
  `ubuntu-latest` freely since neither ships a binary. See
  `supply-drop-bbs-6x6` / `supply-drop-bbs-mnc` (#239, #261) for the
  incident and follow-up this convention exists to prevent from recurring
  in some future release-shipping workflow.
- **Third-party GitHub Actions are pinned by version tag, not commit SHA** —
  a deliberate, repo-wide choice (`supply-drop-bbs-cgy` / #264), not an
  oversight. GitHub's own hardening guidance recommends SHA-pinning, and
  it's a real supply-chain improvement; the tradeoff is a manual-maintenance
  burden this repo doesn't currently have tooling for (no
  `.github/dependabot.yml` `github-actions` ecosystem entry exists yet to
  keep SHA pins current). Revisit this stance if that changes. Until then,
  match the existing pattern (`actions/checkout@v4`, `Swatinem/rust-cache@v2`,
  etc.) rather than SHA-pinning ad hoc in just one file.
- **`dtolnay/rust-toolchain@master` is an intentional exception** to both of
  the above — it's a floating ref, but it's the action's own documented
  pattern for passing an explicit toolchain input, not a drift risk like the
  `ubuntu-latest`/`runs-on` case. See the inline comment at each of its call
  sites (`supply-drop-bbs-0b9` / #262).
- **`cross`'s per-target Docker images (`ghcr.io/cross-rs/<target>`) are
  tag-pinned, not digest-pinned** — an accepted, documented risk
  (`supply-drop-bbs-kmt` / #265), consistent with the stance above: the
  `cross` CLI version itself *is* pinned (via `taiki-e/install-action`),
  which bounds which image tags it resolves, but a compromised/repointed
  tag within that range wouldn't be caught by anything in this pipeline
  today. See the `build` job's own comment in `release.yml` for the full
  reasoning; this generally matches this repo's existing accepted-risk
  stance on unverified git/pip content (`install.sh`, `supply-drop-bbs-9bs`
  / #255).

## Code of conduct

Read [CODE_OF_CONDUCT.md](CODE_OF_CONDUCT.md). It applies to every
interaction in this repository, including PR review.

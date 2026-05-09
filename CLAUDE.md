# Supply Drop BBS — Claude instructions

## Pre-commit checklist

Before every `git commit`, run ALL of the following and fix any failures before committing:

```
rustup run 1.88 cargo fmt --all --check
rustup run 1.88 cargo test --workspace
rustup run 1.88 cargo clippy --workspace -- -D warnings
rustup run 1.88 cargo doc --workspace --no-deps --all-features
```

## Commit style

All commits must use [Conventional Commits](https://www.conventionalcommits.org/) format:

```
<type>[optional scope]: <description>
```

Common types: `feat`, `fix`, `docs`, `refactor`, `test`, `chore`, `build`, `ci`

## Rust toolchain

The pinned toolchain is `1.88` (see `rust-toolchain.toml`). Always prefix cargo commands with `rustup run 1.88`.

# Supply Drop BBS — Claude instructions

## Pre-commit checklist

Before every `git commit`, run ALL of the following and fix any failures before committing:

```
rustup run 1.88 cargo fmt --all --check
rustup run 1.88 cargo test --workspace
rustup run 1.88 cargo clippy --workspace -- -D warnings
rustup run 1.88 cargo doc --workspace --no-deps --all-features
```

Be certain to update documentation, where relevant.


## Commit style

All commits must use [Conventional Commits](https://www.conventionalcommits.org/) format:

```
<type>[optional scope]: <description>
```

Common types: `feat`, `fix`, `docs`, `refactor`, `test`, `chore`, `build`, `ci`

## SQLite migrations — NEVER modify applied migrations

Migrations in `crates/bbs-core/migrations/` are **append-only**. Once a migration file has been committed and could have been applied to any database (dev, staging, or production), it must never be edited. sqlx records a checksum of each applied migration; changing the file content breaks the checksum and crashes the server on startup.

Rules:
- **Never edit an existing migration file.** Create a new numbered file instead.
- **Never add rooms, columns, indexes, or seed data to an existing migration.** Add a new migration.
- If you need to undo something a migration did, write a new migration that reverses it.
- The only safe operation on an existing file is fixing a typo in a SQL comment — but even that changes the checksum, so don't do it.

## Rust toolchain

The pinned toolchain is `1.88` (see `rust-toolchain.toml`). Always prefix cargo commands with `rustup run 1.88`.

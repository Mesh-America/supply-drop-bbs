# ADR-0008: TOML config with env var + CLI overrides

- **Status:** Accepted
- **Date:** 2026-05-08
- **Deciders:** Taedryn

## Context

The BBS needs configuration. The format affects:

- How easy it is for operators to write and edit
- How easy it is for us to validate and produce errors
- Whether it composes well with environment-variable overrides
  (12-factor)
- Whether common footguns (silent type coercion, indentation
  errors) hurt operators

Format candidates: TOML, YAML, JSON, custom DSL, or no file
(env-only).

## Decision

**A single TOML file** as the primary configuration, with overrides
layered as:

1. Compiled-in defaults (lowest priority)
2. The TOML config file
3. Environment variables (`SUPPLY_DROP__SECTION__KEY=value`)
4. Command-line flags (highest priority, small set only)

Implementation: `serde` + `toml` for parsing, `figment` for the
overlay logic.

## Rationale

### Why TOML over YAML

- **No silent type coercion.** YAML's `no` parsing as the boolean
  `false` (the "Norway problem") and `yes`/`on`/`off` as booleans
  cause real bugs. TOML strings are strings, booleans are
  booleans, numbers are numbers, no surprises.
- **No indentation traps.** YAML's whitespace sensitivity is a
  known operator pain point.
- **Native to the Rust ecosystem.** `Cargo.toml`, `cargo` config,
  most Rust tooling speaks TOML. Operators already know it from
  using cargo.
- **Comments are well-supported.** `#`-prefixed comments work
  cleanly. Operators can document their own config without
  fighting the format.
- **Schema is explicit in `serde` structs.** Type errors at parse
  time, before any service starts.

### Why TOML over JSON

JSON has no comments. A config file you can't comment is a
config file you can't maintain.

### Why TOML over a custom DSL

We don't need the expressive power. The config is a flat-ish
structure with sections; TOML covers it.

### Why a single file, not a directory

Some projects use `/etc/foo/conf.d/*.conf` fragments. This is
useful when configuration is large and modular, or when packages
contribute their own files. Neither applies here. One file means
operators can grep one place for a setting.

### Why env-var overrides

Twelve-factor friendliness. Operators running under systemd or
Docker often want to inject secrets via environment rather than
editing files. Standard pattern: `SUPPLY_DROP__SECTION__KEY=value`
where double-underscore separates levels.

### Why CLI flags too

Small set only: `--config`, `--data-dir`, log level overrides,
ports. The CLI is for one-off ops scripts and quick debugging,
not the operator's everyday config surface.

## Consequences

### Positive

- **Predictable parsing.** No format-level surprises.
- **Schema is the Rust struct.** New keys go in the struct, get a
  doc comment, and immediately work. No separate schema file to
  keep in sync.
- **Validation at startup.** Bad config errors with file + key +
  reason. No runtime crashes from misconfiguration.
- **Standard 12-factor overlay** for orchestration platforms.
- **Familiarity.** Operators who've used cargo know TOML.

### Negative

- **TOML is less expressive than YAML** for nested structures.
  We'll occasionally feel this when modeling complex config
  trees (e.g., per-plugin route mappings). Workaround: flatten
  where possible; use TOML's table-array syntax `[[plugins.web.routes]]`
  where unavoidable.
- **Operators expecting YAML may grumble.** Common in some
  ops cultures. We document the choice and move on.

## Operator-facing tools

The BBS binary ships these subcommands for config workflows:

- **`supply-drop-bbs init`** — interactive first-run that
  generates a starting config based on prompts. Default for new
  operators.
- **`supply-drop-bbs config check [--config PATH]`** — validate
  without starting. Exits 0 on success, non-zero with a clear
  error message on failure. For ops scripts and pre-deploy gates.
- **`supply-drop-bbs config show [--config PATH]`** — print the
  effective config (after merging defaults, file, env, CLI). Tells
  operators what's actually in effect, which is often surprising.

The example config (`config.example.toml`) documents every
available knob. A real-world deployment config will be a small
fraction of it (paths, ports, sysop bootstrap), with everything
else inheriting defaults.

## Validation rules

The startup config validation enforces, at minimum:

- **TOML well-formedness.** Errors point at file:line:col.
- **Required keys present.** Every `serde` field without a
  `#[serde(default)]` is required.
- **Type correctness.** Strings are strings, integers are
  integers, ranges are checked.
- **Cross-reference consistency.** E.g., if config references a
  plugin name, that plugin must be enabled at compile time.
- **Permission/ownership of secret-bearing files.** Files
  containing secrets (sysop hashes, future API keys) must be
  mode 0600 or stricter on Unix; warn loudly otherwise.

Validation failure exits the process with a clear error. No
partial startup, no fallback to defaults that mask the problem.

## Future considerations

- **Config reload at runtime.** Some keys can't change at runtime
  (DB path, listen ports). Many can. Worth doing properly when
  there's evidence operators need it; deferred for v1.
- **Schema migration.** Renaming a config key in a future version
  requires either continued support for the old name (preferred)
  or a documented migration step. Each schema change gets its own
  ADR if non-trivial.

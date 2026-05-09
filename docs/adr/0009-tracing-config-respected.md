# ADR-0009: Tracing-based logging that respects config

- **Status:** Accepted
- **Date:** 2026-05-08
- **Deciders:** Mesh-America

## Context

The mesh-citadel project shipped with a logging system that, in
practice, didn't respect the operator's configured log level. The
specific failure: the `--debug` CLI flag silently overrode
`log_level` in config, with no announcement, so an operator who
set `log_level = "WARN"` and forgot they had `--debug` in a shell
alias got a 14 MB/hour DEBUG flood and concluded that the config
"didn't work."

This is a category of bug worth designing against from day 1, not
fixing later. Logging is one of the few things every operator
touches.

## Decision

Use the **`tracing`** crate (and `tracing-subscriber` for output
configuration). Encode the following as hard rules:

1. **The configured log level is the effective log level.** No
   silent overrides. Period.
2. **Any CLI override is announced.** If `--log-level=debug` is
   passed and config says `WARN`, a WARN-level log line at startup
   says exactly that: "CLI override: log level was WARN, now
   DEBUG." Goes to both stderr and the log file.
3. **The first log line is always a level summary.** A WARN-level
   line at process startup states the effective level for every
   target. WARN ensures it survives any reasonable filter.
4. **Per-target overrides are explicit.** Noisy crates
   (companion-frame at frame level, sqlx at query level) clamp to
   WARN by default. Operators who want them louder set explicit
   `[logging.targets]` entries in config.
5. **No `env_logger`-style `RUST_LOG=debug` blanket override.**
   `RUST_LOG` is recognised but routed through the same
   override-announcement path as the CLI flag.

## Rationale

### Why `tracing` over `log` + `env_logger`

- **Structured fields.** `tracing` supports per-event structured
  fields (`session_id=abc, transport=mesh`) natively. The `log`
  crate flattens everything to a string.
- **Spans.** Request flows can be wrapped in spans whose fields
  apply to all events inside them. Critical for debugging
  cross-component flows like "what did this command do."
- **JSON output is a first-class citizen** via
  `tracing-subscriber::fmt::Layer`. Operators piping to log
  aggregators get well-formed structured output with no extra
  work.
- **The `log` crate's events route through `tracing`** via a
  compatibility shim, so dependencies that use `log` (almost
  every Rust library) still work.
- **Better default for new projects.** The Rust ecosystem has
  largely converged on `tracing` for new code. `log` remains
  fine for libraries; applications go `tracing`.

### Why explicit override announcement

The mesh-citadel post-mortem made this exact bug a priority. The
fix in that codebase was retroactive (announce overrides loudly).
Doing it from scratch costs nothing.

The principle: **operator surprise is a bug.** If the system is
doing something the operator didn't ask for, the system tells the
operator.

## Implementation outline

```rust
// Pseudocode for the startup wiring
fn init_logging(config: &LoggingConfig, cli: &CliArgs) -> Result<()> {
    let configured_level = config.log_level;
    let effective_level = cli.log_level_override
        .or_else(|| std::env::var("RUST_LOG").ok().and_then(parse))
        .unwrap_or(configured_level);

    if effective_level != configured_level {
        // ANNOUNCE this loudly. Both stderr (visible immediately)
        // and the log file (persistent record).
        eprintln!(
            "WARN: log level override active: config={configured_level}, \
             effective={effective_level} (source: {source})"
        );
    }

    let filter = build_filter(effective_level, &config.targets)?;

    let subscriber = tracing_subscriber::registry()
        .with(filter)
        .with(file_layer(&config.file)?)
        .with(stderr_layer());

    subscriber.try_init()?;

    // First log line: WARN-level effective-config summary.
    tracing::warn!(
        target: "supply_drop_bbs::startup",
        configured = %configured_level,
        effective = %effective_level,
        targets = ?config.targets,
        "logging initialised"
    );

    Ok(())
}
```

## Consequences

### Positive

- **No more silent overrides.** Operator-set log level is the
  ground truth unless explicitly overridden, and overrides are
  visible.
- **Structured logs from day 1.** Switch to JSON output with one
  config change.
- **Per-target tuning is first-class.** Noisy targets are
  configured, not buried in code as hard-coded clamps.
- **Spans give us request tracing for free.** Useful for
  debugging plugin-to-plugin call flows.

### Negative

- **`tracing` is a slightly larger dependency** than `log`. Not
  meaningful at our binary size scale.
- **Slight learning curve** for contributors used to `log!()`
  macros - but `tracing` provides drop-in compatibility (e.g.,
  `tracing::info!()` works the same way).

### Neutral

- We standardise on `tracing::Level` (TRACE, DEBUG, INFO, WARN,
  ERROR) rather than introducing custom levels. Five is enough.

## Logging targets

The default per-target levels (clamped from root):

| Target prefix              | Default | Notes                          |
|----------------------------|---------|--------------------------------|
| `supply_drop_bbs::*`       | inherit | Our own code; root level       |
| `meshcore_companion::frame` | WARN   | Per-frame trace; very noisy   |
| `sqlx::query`              | WARN    | Per-query trace; very noisy   |
| `hyper`, `tower`, `axum`   | WARN    | Web framework internals       |

Operators override individual targets via:

```toml
[logging.targets]
"supply_drop_bbs::transport::mesh" = "DEBUG"
"sqlx::query" = "INFO"
```

## What this prevents

The May 8 mesh-citadel incident's logging confusion came from:

1. `--debug` CLI flag silently stomping config → **prevented** by
   the announcement rule.
2. Noisy 3rd-party crates clamp skipped at DEBUG root level →
   **prevented** by always-on per-target clamping.
3. No way to see what level was actually in effect → **prevented**
   by the WARN-level startup summary.

The category of "operator says config doesn't work" goes away
structurally.

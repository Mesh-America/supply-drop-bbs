# ADR-0003: Web admin UI as a plugin

- **Status:** Accepted
- **Date:** 2026-05-08
- **Deciders:** Taedryn

## Context

The BBS will ship a web admin interface for sysop maintenance. The
question is whether it's:

a) A first-class feature of the host binary, hard-wired into the
   architecture
b) An optional module enabled via cargo features, but otherwise
   integrated like any other piece of the host
c) A plugin against the same `bbs-plugin-api` that third-party
   transports and extensions use

mesh-citadel took the "first-class" path: the web UI was wired
directly into the transport manager and shared significant code
with the BBS core. This worked but it meant the plugin contract
was theoretical — there was no real example of an extension built
against it, so the contract drifted from what extensions would
actually need.

## Decision

The web admin UI ships as **plugin (option c)**. It implements the
same `TransportEngine` trait as `bbs-cli` and `bbs-mesh`, and uses
the same `Host` interface.

The plugin lives in `crates/bbs-web/`. It's enabled via the
`admin-web` cargo feature. Default: **off.**

## Alternatives considered

### Option (a) — first-class

Rejected. The plugin contract becomes vapor if there's no non-trivial
example. We'd have a `Plugin` trait that no real plugin uses, and
six months in, the trait would be subtly wrong for the first
extension someone wrote.

### Option (b) — opt-in but integrated

Rejected for the same reason. "Optional" doesn't validate the
contract; "implemented against the contract" does.

## Consequences

### Positive

- The plugin API is **forced to be expressive enough** to support
  a real, complex feature: HTTP routes, static-file mounts,
  authentication integration, event subscriptions, config
  validation, lifecycle hooks. If `bbs-web` can be a plugin,
  almost any plausible extension can be.
- **Default-off is enforced naturally.** Cargo features mean
  operators who don't want the web UI don't even compile it in.
  Smaller binary, less attack surface.
- **The web UI doesn't get special privileges.** It can't bypass
  permission checks, can't access the DB directly, can't read
  session tokens it shouldn't see — because it has the same
  `Host` interface as a third-party plugin would.
- **Third-party UI projects** (mobile app, terminal client,
  alternative web frontend) consume the same OpenAPI surface
  the web admin uses. The reference implementation is right
  there.

### Negative

- **Slightly more complex plugin API.** The trait has to support
  static-file mounts and `axum::Router` contributions, not just
  raw command processing. This is the cost of forcing the API to
  be real.
- **Boundary discipline required.** The web plugin must not
  reach into `bbs-core` private APIs as a shortcut. Code review
  enforces this; rustdoc visibility (`pub(crate)` vs. `pub`)
  helps.
- **Slower iteration in some cases.** A change that affects both
  `bbs-core` and `bbs-web` is a coordinated change across two
  crates rather than one. In practice this is fine — small,
  stable interfaces don't require coordinated changes often.

### Neutral

- The web plugin can declare dependencies on other plugins. For
  example, certain admin features may require the mesh transport
  to be loaded (e.g., "send a test mesh DM"). The supervisor
  resolves these at startup and refuses to start if dependencies
  aren't satisfied.

## Notes

This decision validates by construction. The first time we find
something the web plugin needs and the plugin API doesn't support,
that's a signal to extend the API — and that extension benefits
every other plugin.

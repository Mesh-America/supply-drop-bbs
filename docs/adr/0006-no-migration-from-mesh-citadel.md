# ADR-0006: No migration from mesh-citadel

- **Status:** Accepted
- **Date:** 2026-05-08
- **Deciders:** Mesh-America

## Context

Supply Drop BBS is the spiritual successor to
[`mesh-citadel`](https://github.com/taedryn/mesh-citadel), the
Python implementation of the same idea. There's a question whether
to provide a migration path from existing mesh-citadel deployments:

a) Read mesh-citadel's SQLite schema directly and present a
   compatibility layer
b) Ship a one-shot exporter / importer tool that converts a
   mesh-citadel DB into Supply Drop BBS's format
c) No migration. Operators who want to switch start from a fresh
   install.

## Decision

**Option (c): no migration.** Supply Drop BBS does not read,
import, or share schema with mesh-citadel. Operators switching
from one to the other start fresh.

## Rationale

The maintainer's explicit position: this rewrite is intended as a
clean break. Constraints baked into the mesh-citadel schema (the
v1 FK-without-ON-DELETE issues, the in-memory-DB design's effects
on schema choices, the workflow encoding) shaped that codebase
in ways we want to leave behind.

Specific reasons to refuse a migration path:

1. **Schema differences are intentional.** Supply Drop BBS will
   make different choices about timestamp formats (RFC3339 in UTC
   throughout, no naive datetimes), permission level encoding
   (enums with explicit values), workflow state representation
   (typed Rust enums rather than free-form JSON), audit logging
   (explicit table from day 1). Each is a design decision. A
   migration layer would force compromises on those decisions
   to preserve compatibility.

2. **Migration is a long-term commitment.** If we ship a migrator,
   we own its correctness across all future schema evolutions.
   The maintenance cost over years is substantial — and it
   benefits a small number of operators (everyone who's already
   running mesh-citadel) at the expense of every future feature
   decision.

3. **The deployment scale doesn't warrant it.** A single
   mesh-citadel deployment is at most a few hundred users and a
   few thousand messages. Operators who want to switch can re-create
   their rooms (a few minutes of work) and let users re-register.
   Existing message history is lost; for a mesh BBS where messages
   are short-form and short-lived, this is acceptable.

4. **It clarifies the project's relationship to its predecessor.**
   "Supply Drop BBS is a fresh project" is a cleaner story than
   "Supply Drop BBS is mesh-citadel v2." The two projects can
   evolve independently. Operators choose; nobody is migrated
   without consenting to the rewrite.

## Consequences

### Positive

- **No migration code to maintain.** Saves ongoing engineering cost.
- **Clean schema design.** Every schema choice is on its own merits,
  not constrained by mesh-citadel compatibility.
- **Clearer project boundaries.** Contributors and operators
  understand exactly what this project is and isn't.
- **mesh-citadel can continue.** It exists, it works, and operators
  who are happy with it don't have to switch. The two projects
  coexist.

### Negative

- **Operators switching lose their data.** Specifically: existing
  rooms must be recreated, users re-register, message history is
  lost. We document this cost prominently.
- **No partial-rollout option.** An operator can't run both
  side-by-side with shared state to compare. They can run both
  with separate state, which is fine for evaluation but not
  literal A/B.

### Neutral

- A community member who really wants a migration path is welcome
  to write one as a separate tool. We won't endorse it or
  maintain it, but we won't actively prevent it. This is the
  pragmatic compromise: anyone who needs the feature badly enough
  can build it themselves.

## Notes for operators

If you're on mesh-citadel today and considering Supply Drop BBS:

- Treat this as a fresh deployment. Plan accordingly.
- If you have valuable message history, export it from
  mesh-citadel first (it has its own export tooling) and keep
  the archive for reference.
- Re-create your rooms in Supply Drop BBS. Document the room
  layout you want first; setting up 5-20 rooms takes minutes.
- Communicate with your users that they'll need to re-register
  on the new BBS. The mesh nodes that already know the old BBS
  may still work fine for some time depending on how you swap
  the deployment.
- If both BBSes need to coexist temporarily, run them on
  different mesh node identities so the radio-side state
  doesn't collide.

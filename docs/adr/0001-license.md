# ADR-0001: Apache 2.0 + Commons Clause license

- **Status:** Accepted
- **Date:** 2026-05-08
- **Deciders:** Mesh-America

## Context

The project's license affects: who can use it, who can contribute,
which dependencies are compatible, and what business models third
parties can build on top.

The maintainer's stated goals:

- Anyone should be able to use and modify the software
- No one should be able to make a profit from selling the software
  itself, or from selling hosted services derived primarily from
  its functionality

These goals are mutually incompatible with OSI-approved open source.
The OSI definition explicitly forbids "discrimination against fields
of endeavor" — i.e., you cannot say "you may not use this
commercially." The license has to live outside the OSI tent.

## Decision

License the project under **Apache License 2.0 with the Commons
Clause v1.0** appended as a restriction. The full license text lives
in [LICENSE](../../LICENSE).

## Alternatives considered

### MIT, BSD-3-Clause, Apache 2.0 (without Commons Clause)

OSI-approved permissive licenses. All explicitly allow commercial
use and resale. Rejected because they fail the "no profit from
selling" requirement.

### GPL family (GPLv3, AGPLv3)

OSI-approved copyleft. Allows commercial use; requires source
disclosure for distributions (and for AGPL, network use). Doesn't
prevent profit; just requires sharing modifications. Rejected for
the same reason as the permissive licenses.

### PolyForm Noncommercial 1.0.0

Source-available, written in plain English, drafted by serious IP
lawyers. Forbids any commercial use. Considered closely. Rejected
as broader than the maintainer's actual concern: PolyForm NC blocks
"I run this BBS for my coworkers and we are a for-profit company"
even though no resale is happening. The Commons Clause is narrower
— it specifically blocks resale, not commercial use generally.

### Forklift Certified License (mesh-citadel's existing license)

Idiosyncratic. Has a resale clause but also adds employer-pay-ratio
caps, headcount caps, and an industry-exclusion list. Maintainer
explicitly wanted "least restrictive possible," so the FCL's scope
expansion is wrong for this project.

### BSL (Business Source License)

Commercial-restriction license that auto-converts to a permissive
license after a configurable time delay (typically 4 years). The
auto-conversion is interesting but adds complexity and uncertainty
for downstream users planning long-running deployments. Rejected
as overkill for the maintainer's stated goal.

### Functional Source License (FSL)

Similar to BSL, simpler. Auto-converts to MIT or Apache 2.0 after
2 years. Rejected for the same reason as BSL: complexity not
warranted by the stated goal.

## Consequences

### Positive

- Anyone can read, fork, modify, and redistribute the code for any
  purpose other than selling it
- Hobbyist sysops, mesh nonprofits, research, education,
  in-house commercial use are all unambiguously permitted
- Apache 2.0's patent grant protects users against patent
  retaliation by contributors
- Apache 2.0 base is well-understood by every IP lawyer; only the
  Commons Clause delta is novel

### Negative

- Not OSI open source. Cannot land in Debian main, Fedora's
  packaging, or Arch's `community` repo. Some FOSS-purist
  contributors will refuse to participate on principle.
- Commons Clause has a contentious history (Redis Labs, MongoDB SSPL).
  Some operators will be wary on reputation grounds even if the
  semantics are fine for their use.
- Cannot be combined with GPL-licensed code. If a future feature
  needs a GPL-only library, we either find an alternative or carve
  the code out behind an FFI boundary.

### Neutral

- The CLA in [CONTRIBUTING.md](../../CONTRIBUTING.md) grants the
  project owner the right to relicense contributions under different
  terms in the future. This is the escape hatch if the license
  proves to be the wrong choice — we can move to PolyForm NC or
  full Apache 2.0 (dropping Commons Clause) later.

## Notes for downstream users

If you want to use Supply Drop BBS for a product or service whose
value derives substantially from its functionality, and you intend
to charge for that — contact the licensor for a separate commercial
arrangement. The Commons Clause text is explicit that this is the
intended carve-out path.

If you're a hobbyist sysop, a school, a research lab, a mesh-radio
nonprofit, a co-op, or even a for-profit company using it internally
without selling access — you're fine under the existing license. No
separate arrangement required.

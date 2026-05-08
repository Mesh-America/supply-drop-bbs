# Security policy

## Reporting a vulnerability

**Do not open a public GitHub issue for security vulnerabilities.**

Instead, contact the maintainer privately. Until a dedicated security
contact is set up, use a GitHub private vulnerability report
(`Security` tab → `Report a vulnerability`) on this repository.

When reporting, please include:

- A description of the vulnerability and its impact
- Steps to reproduce, including configuration and version
- Whether you have a proof-of-concept exploit (do not include working
  exploits in the initial report; we'll request them if needed)
- Whether you've already disclosed it elsewhere
- How you'd like to be credited (or not) when the fix is published

## Disclosure policy

We follow a coordinated-disclosure model:

1. **Acknowledgement** within 7 days of receipt.
2. **Triage and confirmation** within 14 days. We'll tell you whether
   we accept it as a vulnerability and our planned fix timeline.
3. **Fix** developed privately. For most issues we aim for under 30
   days; complex or systemic issues may take longer.
4. **Release** of the fix and a public advisory describing the
   vulnerability, affected versions, and remediation.
5. **Disclosure** of the underlying details no earlier than 90 days
   after the initial report, or sooner with mutual agreement.

If you find a vulnerability that you believe poses immediate, severe
risk to operators in the wild, contact us with `URGENT` in the subject
and we'll prioritise.

## Scope

In scope:

- The Supply Drop BBS binary and its bundled plugins (`bbs-core`,
  `bbs-cli`, `bbs-mesh`, `bbs-web`, `bbs-plugin-api`).
- The `meshcore-companion` crate.
- Configuration handling, including any way an attacker could escalate
  privileges, bypass authentication, or read/modify data they shouldn't.
- The audit-log integrity guarantees.
- The wire format and the OpenAPI surface.

Out of scope (report to the upstream project):

- Vulnerabilities in `pymc_core`, `meshcore_py`, or the MeshCore
  firmware itself. Report those at the relevant upstream repos.
- Vulnerabilities in third-party plugins or UIs that consume our
  plugin API. Report to that plugin's maintainers.
- Operating-system or hardware vulnerabilities on the deployment
  target.

## What's not a vulnerability

- Performance regressions, even severe ones (file as a bug).
- Functional bugs without a security impact (file as a bug).
- "Best practice" recommendations without a demonstrated attack
  (welcome as a discussion or PR; not a vulnerability report).
- Issues requiring physical access to the deployment hardware.
- Issues only reproducible against custom-modified builds.

## Recognition

We credit reporters in the release notes for the fix and on a
SECURITY-CREDITS file (when one accumulates), unless the reporter
prefers anonymity.

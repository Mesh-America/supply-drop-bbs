# ADR-0010: OpenAPI generated from Rust via `utoipa`

- **Status:** Accepted
- **Date:** 2026-05-08
- **Deciders:** Mesh-America

## Context

The web admin plugin's HTTP API is the contract that:

- The bundled Vue frontend consumes
- Third-party clients (alternate UIs, mobile apps, terminal clients,
  scripted ops tooling) consume
- Future plugins extend

A formal API specification serves all of those. The candidates:

1. **OpenAPI 3.x specification**, hand-written and maintained
   alongside the Rust code
2. **OpenAPI generated from Rust source** via a derive-style
   library
3. **No formal spec; rustdoc serves as the API documentation**
4. **GraphQL** instead of REST + OpenAPI

## Decision

**OpenAPI 3.1, generated from Rust via the `utoipa` crate.** Output
committed to `docs/openapi.json`. Live `/openapi.json` endpoint
served by the web admin plugin so live verification matches the
committed spec.

## Rationale

### Why OpenAPI rather than GraphQL

GraphQL is a fine choice for some applications. Not this one:

- **Surface is small and well-shaped.** The admin API is dozens
  of endpoints, not thousands; REST works.
- **Cache headers and HTTP semantics matter.** Reports can be
  cached; deletes return 204. REST gives this for free.
- **Tooling.** The Rust + axum + OpenAPI stack is mature.
  GraphQL Rust libraries are good but overhead is not free.
- **Operator familiarity.** Most operators know HTTP. Fewer know
  GraphQL.

### Why generated rather than hand-written

The risk of a hand-written spec is **drift.** Code adds a field;
spec doesn't. Spec says a field is required; code accepts null.
Anyone consuming the spec gets bitten.

Generating the spec from the same Rust types and route handlers
that implement the API means **the spec can't lie.** If the code
compiles, the spec describes what the code does.

### Why `utoipa` specifically

- Active development, axum 0.7+ support
- Derive-macro-driven; types get `#[derive(ToSchema)]` and routes
  get `#[utoipa::path(...)]` attributes — no separate definitions
- Generates valid OpenAPI 3.1
- Plays well with Swagger UI and Redoc
- Handles enums, generics, optional fields, custom serialisers
  cleanly

`aide` is the alternative; mature too, but with a slightly heavier
type-driven approach. `utoipa`'s derive style is closer to the
ergonomics of the rest of our stack.

### Why commit the spec

Three reasons:

1. **Diffability.** A reviewer can see API changes in PR diffs
   without running the build. Catches accidental breaking changes.
2. **Discoverability.** Anyone landing on the repo can read
   `docs/openapi.json` without running anything.
3. **Tooling.** Some tools (codegen for client SDKs, mock
   servers, contract tests) consume a static file.

CI verifies that `docs/openapi.json` matches the build-time
generated spec. Out-of-sync = build fails.

## Implementation outline

```rust
// In bbs-web/src/types.rs
#[derive(Serialize, Deserialize, ToSchema)]
pub struct UserSummary {
    pub username: String,
    pub display_name: String,
    pub permission_level: PermissionLevel,
    pub status: UserStatus,
}

// In bbs-web/src/handlers.rs
#[utoipa::path(
    get,
    path = "/api/v1/users",
    responses(
        (status = 200, body = Vec<UserSummary>),
        (status = 401, body = Error),
        (status = 403, body = Error),
    ),
    tag = "users",
)]
async fn list_users(/* ... */) -> Result<Json<Vec<UserSummary>>, Error> {
    // ...
}

// In bbs-web/src/main.rs (or wherever the plugin assembles routes)
#[derive(OpenApi)]
#[openapi(
    paths(handlers::list_users, /* ... */),
    components(schemas(UserSummary, /* ... */)),
)]
pub struct ApiDoc;
```

The plugin's static-file routes:

- `/openapi.json` — live spec
- `/api/docs` — Swagger UI

The CI step: `cargo run --bin gen-openapi -- --check` compares
the generated spec to the committed one. PR fails on drift.

## Consequences

### Positive

- **Spec can't drift from code.** If the code compiles, the spec
  is accurate.
- **Versioning is explicit.** Path prefix `/api/v1/` makes
  breaking changes visible. Non-breaking additions are detected
  automatically by the diff against committed spec.
- **Third-party UIs and clients have a contract.** Anyone wanting
  to write a different frontend has the spec to work against.
- **Codegen is possible.** Operators who want a typed Python or
  TypeScript client can generate one with `openapi-generator`
  from our committed spec.
- **Swagger UI for free.** Interactive API exploration ships in
  the binary at no real cost.

### Negative

- **`utoipa` adds compile time** to the web crate. Acceptable.
- **Derive macros require careful type design** — types that work
  for serde may need additional annotations to generate good
  OpenAPI. We document the patterns as we encounter them.
- **OpenAPI 3.1 is recent.** Some downstream tools still target
  3.0. We default to 3.1 and document the version in the spec.

### Neutral

- The mesh transport's wire format is **not** OpenAPI. That's a
  binary protocol documented separately in
  [`PROTOCOL.md`](../PROTOCOL.md). OpenAPI applies only to the
  HTTP/REST surface.

## Versioning

API paths are prefixed `/api/v1/`. Rules:

- **Adding a field to a response.** Non-breaking. New OpenAPI
  commit; same `/v1/` prefix.
- **Adding an optional field to a request.** Non-breaking.
- **Adding a new endpoint.** Non-breaking.
- **Removing a field, changing a field's type, removing an
  endpoint, making an optional field required.** Breaking. New
  endpoint goes at `/api/v2/...`; the `/v1/` endpoint continues
  to work for at least one minor release with a `Deprecation`
  header pointing at the v2 replacement.

This isn't unlimited backwards-compat — we're not promising
twenty years of v1 support. Two release cycles is enough for
operators to update their clients.

## Future considerations

- **gRPC for plugin-to-host calls?** Probably not — `Host` is a
  Rust trait, no need for serialisation. But if WASM plugins
  arrive, we'll need a wire protocol for that boundary.
- **Generated client SDKs** as part of the release. Currently
  third-party developers run `openapi-generator` themselves;
  in the future we may publish official Python and TypeScript
  clients.
- **Contract tests** that hit a running BBS and verify behaviour
  against the spec. Beyond CI's spec-drift check.

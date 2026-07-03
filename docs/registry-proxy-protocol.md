# Registry proxy wire protocol (v1)

The **registry proxy** is the HTTP service `karac`'s package manager fetches
dependencies through (default `https://proxy.kara-lang.org`, overridable via
`KARAC_REGISTRY_PROXY`). This document ratifies the wire contract between the
client ([`src/registry_proxy.rs`](../src/registry_proxy.rs) — `HttpProxyClient`)
and any conforming server. The reference server in
[`registry-proxy/`](../registry-proxy/) implements exactly this contract and is
the executable definition of it.

This closes the registry-proxy follow-up (b) ("Wire protocol definition") in
[`docs/implementation_checklist/phase-5-diagnostics.md`](implementation_checklist/phase-5-diagnostics.md).

## Transport

- Plain HTTP or HTTPS. The client uses `ureq`; HTTPS is expected in production,
  plain HTTP is used for local/dev mirrors and tests.
- All endpoints are `GET`. Any other method is `405 Method Not Allowed`.
- The client joins endpoint paths onto the configured base URL after stripping a
  trailing `/`.

## Authentication

The public `proxy.kara-lang.org` is unauthenticated. A **private** mirror may
require a bearer token: when `KARAC_REGISTRY_TOKEN` is set (non-empty), the
client sends it on **every** request as:

```
Authorization: Bearer <token>
```

The token is per-user and **never** read from `kara.toml` (only from the
environment), so a credential is never committed. An empty / whitespace-only
`KARAC_REGISTRY_TOKEN` is treated as unset (no header sent). A proxy that
rejects the request with `401 Unauthorized` or `403 Forbidden` surfaces as
`Unauthorized { url, status }` (`E_PROXY_UNAUTHORIZED`) — applies to both
endpoints below.

## Endpoint: catalog

```
GET <base>/catalog/<name>
```

Returns the version catalog for package `<name>` as JSON:

```json
{
  "upstream": "https://github.com/serde-rs/serde",
  "versions": ["1.0.0", "1.2.3", "1.3.0"],
  "yanked": ["1.2.3"]
}
```

| Field      | Type       | Meaning                                                        |
|------------|------------|----------------------------------------------------------------|
| `upstream` | string     | The package's original source URL (git/registry). Package-level. |
| `versions` | string[]   | Every published version, each a valid [SemVer](https://semver.org) string. |
| `yanked`   | string[]   | *Optional.* Versions the publisher has withdrawn (see below). Absent → none. |

Client mapping → `FetchedManifest { package, upstream_url, versions, yanked }`.

**Yanked versions.** A yanked version is still published — its tarball still
resolves, so a `kara.lock` that already pins it keeps building — but it is
**excluded from fresh version selection**: a new resolve never picks a yanked
version. If the *only* versions satisfying a dependency's requirement are
yanked, the client fails with `E_REGISTRY_ONLY_YANKED` (a distinct, clearly
worded diagnostic) rather than a misleading "no matching version". Each `yanked`
entry is a SemVer string validated exactly like a `versions` entry; an entry
need not also appear in `versions` (a yanked-and-delisted version is tolerated).

**Status codes**

| Status  | Client result                                    |
|---------|--------------------------------------------------|
| `200`   | Parsed as above.                                 |
| `401` / `403` | `Unauthorized { url, status }` (missing/invalid `KARAC_REGISTRY_TOKEN`). |
| `404`   | `PackageNotFound { name }`                        |
| other   | `MalformedResponse` (unexpected status).          |

A `200` whose body is not valid JSON, is missing `upstream` (string) or
`versions` (array), has a `yanked` value that is not an array, or contains a
non-string / unparseable-SemVer entry in either `versions` or `yanked`, surfaces
as `MalformedResponse`.

## Endpoint: package tarball

```
GET <base>/pkg/<name>/<version>.tar.gz
```

Returns the gzip-compressed tarball bytes for one concrete version.

**Response headers**

| Header                | Value                | Meaning                                     |
|-----------------------|----------------------|---------------------------------------------|
| `Content-Type`        | `application/gzip`   | The body is the tarball.                     |
| `Karac-Content-Hash`  | `blake3:<64-hex>`    | BLAKE3 digest of the body (same `blake3:<hex>` format as the build cache). Optional but recommended. |

Client mapping → `FetchedPackage { package, version, upstream_url, mirror_url, tarball_bytes, content_hash }`:

- `mirror_url` = the full proxy tarball URL requested.
- `content_hash` = the advertised `Karac-Content-Hash` if present, else the
  digest the client computes over the received body.
- `upstream_url` is **not** carried by this endpoint (it is a package-level
  attribute delivered by `/catalog`); the client leaves it empty here, and the
  resolver stitches it into `kara.lock` from the catalog manifest.

**Integrity check.** When `Karac-Content-Hash` is present, the client computes
the BLAKE3 digest of the body it received and refuses the transfer with
`MalformedResponse` if it does not match — a corrupted or tampered tarball is
never cached.

**Status codes**

| Status  | Client result                                    |
|---------|--------------------------------------------------|
| `200`   | Tarball, hash-verified as above.                  |
| `401` / `403` | `Unauthorized { url, status }` (missing/invalid `KARAC_REGISTRY_TOKEN`). |
| `404`   | `VersionNotFound { name, version }`               |
| other   | `MalformedResponse` (unexpected status).          |

## Transport failures

Any failure to reach the proxy (DNS, connection refused, TLS, timeout) surfaces
as `Unreachable { url, message }` on either endpoint.

## Configuring the proxy URL

The effective proxy URL is resolved by `registry_proxy::ProxyConfig::resolve`,
highest precedence first:

1. the `KARAC_REGISTRY_PROXY` environment variable (when non-empty);
2. the project's `[build].registry-proxy` pin in `kara.toml`;
3. the built-in default (`https://proxy.kara-lang.org`).

```toml
# kara.toml — pin a mirror for the whole project (no per-shell export needed)
[build]
registry-proxy = "https://mirror.internal.example/kara"
```

The manifest value must be a non-empty `http://` / `https://` URL; a malformed
value is a parse error rather than a silent fallback to the default.

## Not covered by v1

These are tracked as registry-proxy follow-ups in the phase-5 checklist and do
not change the contract above when they land:

- Upstream mirroring / the server actually fetching from the origin (server-side).
- Multi-mirror / high-availability (d) and signature verification (f) — a
  signature would be a sibling field to `content_hash`. (Authentication (e) is
  now specified above under **Authentication**.)
- Per-package proxy override (i) and `--no-proxy` direct-from-source fetch (j/k).
- Publisher-side yanking — the `karac yank` *command* that marks a version
  withdrawn (l). The catalog's `yanked` array and the client's honoring of it
  are specified above under **Endpoint: catalog**; what remains deferred is the
  publish-side tooling that writes that array, and honoring a `kara.lock` pin of
  an already-yanked version with a warning (needs lockfile-pin-over-catalog
  precedence, not yet implemented — today a fresh resolve simply refuses).

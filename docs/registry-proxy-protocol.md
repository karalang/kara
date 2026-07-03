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

## Endpoint: catalog

```
GET <base>/catalog/<name>
```

Returns the version catalog for package `<name>` as JSON:

```json
{
  "upstream": "https://github.com/serde-rs/serde",
  "versions": ["1.0.0", "1.2.3", "1.3.0"]
}
```

| Field      | Type       | Meaning                                                        |
|------------|------------|----------------------------------------------------------------|
| `upstream` | string     | The package's original source URL (git/registry). Package-level. |
| `versions` | string[]   | Every published version, each a valid [SemVer](https://semver.org) string. |

Client mapping → `FetchedManifest { package, upstream_url, versions }`.

**Status codes**

| Status  | Client result                                    |
|---------|--------------------------------------------------|
| `200`   | Parsed as above.                                 |
| `404`   | `PackageNotFound { name }`                        |
| other   | `MalformedResponse` (unexpected status).          |

A `200` whose body is not valid JSON, is missing `upstream` (string) or
`versions` (array), or contains a non-string / unparseable-SemVer version entry,
surfaces as `MalformedResponse`.

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
- Multi-mirror / high-availability (d), authentication (e), signature
  verification (f) — a signature would be a sibling field to `content_hash`.
- Per-package proxy override (i), `--no-proxy` direct-from-source fetch (j/k),
  and `karac yank` status surfacing (l).

# kara-registry-proxy

A **reference / dev** registry proxy for the Kāra package manager. It serves
package catalogs and tarballs over HTTP so `karac` can fetch dependencies from
it (via `KARAC_REGISTRY_PROXY`). It implements the wire protocol in
[`docs/registry-proxy-protocol.md`](../docs/registry-proxy-protocol.md).

> **Not the production mirror.** It serves packages straight off a local
> directory — no upstream mirroring, caching, auth, signatures, or HA. Use it
> for local development, a private/internal mirror, tests, or as the executable
> definition of the protocol.

## One-command local proxy (tier 1)

Lay out a folder of packages — one subdirectory per package, a
`<version>.tar.gz` per release, and an optional `upstream` file with the source
URL:

```text
pkgsrc/
  mylib/
    1.0.0.tar.gz
    1.2.0.tar.gz
    upstream          # optional: https://github.com/me/mylib
```

Build a servable store, then serve it:

```bash
# assemble pkgsrc/ into a store/ (generates catalogs, sorts versions by SemVer)
cargo run -p kara-registry-proxy -- build --from pkgsrc --out store

# serve it
cargo run -p kara-registry-proxy -- serve --root store --port 8080

# point karac at it (another shell)
export KARAC_REGISTRY_PROXY=http://127.0.0.1:8080
```

That's a fully working proxy for one machine. For a team, run `serve` on a shared
host and point everyone at its address (add a TLS-terminating reverse proxy such
as nginx/Caddy for HTTPS).

## Endpoints

- `GET /catalog/<name>` → `{ "upstream": "...", "versions": [...] }`
- `GET /pkg/<name>/<version>.tar.gz` → the tarball, with a
  `Karac-Content-Hash: blake3:<hex>` header the client verifies.

## Commands

```text
kara-registry-proxy serve --root <DIR> [--addr <IP>] [--port <N>]
kara-registry-proxy build --from <DIR> --out <DIR>
```

`build` sorts versions as real SemVer (so `1.10.0` orders after `1.9.0`) and
rejects a mis-named `<not-semver>.tar.gz` rather than dropping it silently.

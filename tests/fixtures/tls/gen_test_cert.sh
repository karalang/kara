#!/bin/sh
# Phase 6 line 236 slice 4 — test-cert regeneration.
#
# Regenerates the checked-in `cert.pem` + `key.pem` self-signed
# certificate used by TLS-related tests under `tests/` and bench
# harnesses under `examples/ws_idle_holder` (Demo 1).
#
# **Usage:** `cd tests/fixtures/tls && ./gen_test_cert.sh`
#
# **Validity:** 3653 days = ~10 years. The 10-year window dodges the
# "tests fail in 2027" trap when a 1-year cert quietly expires
# mid-development; the long window lets the v1 launch through M3 ride
# on the same fixtures without a forced regeneration.
#
# **Subject / SAN:** CN=localhost with both DNS:localhost and
# IP:127.0.0.1 SubjectAltNames. The dual SAN lets the same cert serve
# `wss://localhost:<port>` from rustls clients (which check DNS SANs by
# default) AND raw IP-address connection paths from bench harnesses
# that bypass DNS resolution. Modern TLS clients (rustls included)
# require the SAN — relying on the legacy `CN` field alone produces
# `UnsupportedNameType` rejections.
#
# **Dependencies:** OpenSSL CLI. Tested with the system openssl on
# macOS / Linux. The fixtures themselves are checked in so contributors
# who don't have openssl available can still run tests against the
# committed PEM bytes — this script is for the rare regen case
# (post-expiry, or wanting to validate the recipe).
#
# **What this is NOT:** a production cert. The private key is
# committed to git; anyone with the repo can forge a server identity
# for `localhost`. This is fine for tests against `127.0.0.1` but
# would be a security disaster if deployed to any reachable address.

set -eu

cd "$(dirname "$0")"

openssl req \
    -x509 \
    -newkey rsa:2048 \
    -keyout key.pem \
    -out cert.pem \
    -days 3653 \
    -nodes \
    -subj "/CN=localhost" \
    -addext "subjectAltName=DNS:localhost,IP:127.0.0.1"

echo "Regenerated test cert + key:"
openssl x509 -in cert.pem -noout -subject -enddate -ext subjectAltName

# mini_oauthlib

OAuth request signing and the client side of OAuth 2, as oauthlib 3.2.2
does them (the version Ubuntu 24.04 ships, and the library underneath
requests-oauthlib). It is ported from the `oauthlib` package vendored
here (BSD-3-Clause, see `LICENSE-oauthlib`), together with the parts of
Python it rests on.

**OAuth 1** (RFC 5849) covers `escape` and `unescape`, `urldecode` with
its character and hex checks, the Authorization header parser (quoted
commas, escaped quotes, a repeated key, and what it rejects), and the
signature pipeline: the base string URI (scheme and host lowercased,
default ports dropped, a `Host` override, IPv6 hosts written the way
`ipaddress` prints them, and every port error), parameter collection
from the query, the header and the body, normalization, the base string,
and HMAC-SHA1, HMAC-SHA256, HMAC-SHA512 and PLAINTEXT signatures.
`Client.sign` runs its sanity checks with their exact messages, adds
`oauth_body_hash` for a body that is not form-encoded, and renders the
signature into the header (with a realm), the query or the body.
oauthlib's quirks are kept: an empty body or a bare word counts as
parameters, a lowercase `host` header is honoured for query and body
signatures but not for header ones, and a body signature adds a second
`Content-Type` key next to a lowercase one. Each signature is checked
again the way a server would, valid and tampered.

**OAuth 2** (RFC 6749) covers `WebApplicationClient` and
`BackendApplicationClient`: the authorization request, PKCE challenges,
the callback (state check, errors, missing code), the token request with
its keyword arguments, token responses in JSON or form encoding (expiry
times, scope lists, scope changes, error responses), bearer tokens in
the header, the query or the body, MAC tokens with HMAC-SHA1 and
HMAC-SHA256, expired tokens and clients built from saved tokens, refresh
and revocation requests. The error classes are rebuilt from their codes
the way `raise_from_error` picks them, with their JSON, form, header and
redirect forms.

Underneath are `urllib.parse`, `urllib.request.parse_http_list`,
`ipaddress`, `hmac` with SHA-1, SHA-256 and SHA-512, `base64`, `json`
and Python's `repr()`.

The run writes `data/` and works through `data/jobs.jsonl`. It ends
with an audit of `data/requests.log`, a day of signed API calls as a
server logs them, against `data/consumers.tsv`: each request is checked
for a malformed header, missing parameters, an unknown consumer or
token, a timestamp outside the window, a replayed nonce and a bad
signature, and the verdicts are counted by consumer and by reason.

`expected.out` is the output of `oracle.py`, which runs the same jobs
through oauthlib itself with `time.time` fixed. `mk.py` writes `data/`
(it signs the logged requests with oauthlib); `gen_kara.py` prints the
data section at the end of `mini_oauthlib.kara`.

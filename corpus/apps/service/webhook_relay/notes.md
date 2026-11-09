# webhook_relay

A webhook relay in the shape of the ones that sit in front of GitHub or
Stripe webhooks: an ingress that verifies signatures and spools each accepted
delivery to disk, a forwarder that relays the spool to a downstream service
with retries and a dead-letter directory, and the downstream itself. Both
servers run in the program on loopback (ports 47749 and 47750), and the
spool lives under `relay/`, the downstream's state under `down/`.

- **Ingress** (`POST /hooks/<source>`). Each source has its own secret
  (obviously fake ones here). `X-Hub-Signature-256: sha256=<hex>` must be
  the HMAC-SHA256 of the body under that secret, compared in constant time
  (401 when missing or wrong). `X-Delivery-Id` is required and limited to
  letters, digits and `-` (400). The body must be a JSON object with a
  string `event` (422). A delivery id seen before is answered 200
  `duplicate` and not spooled again. An accepted delivery is written to
  `spool/<seq>-<source>-<id>.json` through a temporary file and a rename,
  as a record wrapping the original payload, and answered 202 with its
  sequence number. An unknown source is 404, another method 405, and
  `GET /health` counts the spool.
- **Forwarder.** Relays the spool in order to `POST /ingest`, with
  `X-Relay-Attempt`. A failure is retried up to three attempts with
  exponential backoff; a delivered file is logged to `delivered.log` and
  removed, and one that fails three times moves to `dead/`.
- **Downstream.** Counts attempts per delivery on disk, answers 503 to the
  first attempt of a delivery whose id ends in an odd digit, always 500 to
  a `poison` event, and appends what it accepts to `received.log`, served
  at `GET /received`.

The client sends good deliveries from two sources, a wrong secret, an
unknown source, a missing signature, malformed JSON, a JSON array, a body
without `event`, a redelivery, a poison event, missing and invalid delivery
ids and a `GET`, prints the spool, runs the forwarder and prints the
delivered log, the dead letters and what the downstream received.

Oracle: `webhook_relay.py`, a port over real sockets (`http.server`,
`urllib`, `hmac`). `expected.out` is its output. A status of 400 or more is
an `Err` carrying only a message in the Kāra client (B-2026-11-09-44), so
both sides print that message.

## Edits from webhook_relay.orig.kara to webhook_relay.kara

By `karac fix`, from machine-applicable diagnostics (the diagnostics were
right):
- `||` and `&&` are `or` and `and` (6 sites).
- `message: String` is given away into the reply; the parameter borrows,
  so the fix takes it as `own String`.
- `body.stringify()` takes its receiver by value and `body` is a borrowed
  parameter; the fix stringifies a `.clone()`.
- Style (`redundant_literal_method`): `.to_string()` dropped from string
  literals in `String` slots.

By hand, following the diagnostic's own advice:
- A nested f-string (`f"..{read_text(f"{RELAY}/..")}"`) is deprecated by
  design.md §3; the inner path is bound first.

Compiler gaps this app found (fixed in kara):
- `Json` had no `.clone()`, and the refusal advised adding
  `#[derive(Clone)]` to the library's declaration (B-2026-11-09-48).
- `[("event", s("push")), ..].to_vec()` passed to a `Vec[(Str, Json)]`
  parameter was refused as a `Vec[(String, Json)]` (B-2026-11-09-49).

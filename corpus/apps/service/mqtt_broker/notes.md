# mqtt_broker

An MQTT 3.1.1 broker, a subset, on loopback (`127.0.0.1:47764`). Each
connection is a task that reads packets (a type byte, the variable-length
remaining length, a body); the broker state (sessions, subscriptions,
retained messages, packet counts) is one `Mutex` in a `sync struct`, and
every packet a command produces is written to its recipients' sockets under
that lock, so each client sees messages in the order they were routed.

- **CONNECT.** Protocol `MQTT` level 4, else `CONNACK` code 1 and a close;
  an empty client id without a clean session gets code 2, with one it gets
  `auto-<n>`. A will (topic, payload, QoS, retain) is kept for the session.
- **SUBSCRIBE.** Filters with `+` (one level) and `#` (the rest, last
  only); an invalid one is granted `0x80`. QoS is granted up to 1. The
  retained messages a new filter matches follow the `SUBACK`, in topic
  order, with the retain flag set. `UNSUBSCRIBE` gets `UNSUBACK`.
- **PUBLISH.** QoS 0 and 1. Every subscriber gets a message once, at the
  highest QoS among its matching filters (capped by the message's), with a
  packet id of its own at QoS 1; the sender then gets `PUBACK`. A retained
  message is kept per topic, and an empty payload clears it. A topic with a
  wildcard is a protocol error that closes the connection.
- **Leaving.** `DISCONNECT` drops the will; a connection that ends any
  other way publishes it. `PINGREQ` gets `PINGRESP`.

**Self-test.** The remaining-length encoding at each byte boundary, nine
filters through the validity check, and nine filter/topic pairs through
the matcher.

**Live run.** Six scripted clients on the main task, each step reading
exactly the packets the broker owes it (a 2 s read timeout turns a missing
one into an error in the output instead of a hang): two refused connects,
retained publishes, overlapping filters and an invalid one, QoS 1 delivery
and acknowledgement, a 200-byte payload (a two-byte remaining length), an
unsubscribe, a ping, a will published when its client drops, a wildcard
publish, two disconnects. The broker accepts six connections and ends when
their tasks do, then prints how each ended, the packet counts and the
retained messages.

Oracle: `mqtt_broker.py`, the same broker with `socket` and threads, and
the same scripted clients.

## Porting notes

`mqtt_broker.orig.kara` is the Rust-shaped first draft; `mqtt_broker.kara`
is what `karac fix` made of it plus the hand edits below.

By `karac fix`, from machine-applicable diagnostics: `&`/`&mut` borrows,
`<..>` generics, `::` paths, `println!`/`format!` (with `{:02x}`),
`vec![..]` and `vec![0u8; n]`, `move ||`, `.as_bytes()`, `usize` lengths,
`.to_string()` on literals, `!`/`||` as `not`/`or`, and
`collect::<Vec<String>>().join(" ")` as `Vec[String].from_iter(..)`.

Fixed in kara while writing it:
- `format!("'{}'", String::from_utf8_lossy(&payload))` got no f-string fix:
  a `:` in an argument refused the rewrite (B-2026-11-11-21).
- `u16::from_be_bytes([a, b])` refused its list literal as a `Vec`
  (B-2026-11-11-23).

By hand, around open rows:
- `conn.read_exact(&mut body)?` in `read_packet`: no reader has
  `read_exact` (B-2026-11-10-44); the `.kara` has a helper that loops over
  `read`.
- `read_packet` declared Rust's `io::Result`; a socket fails with
  `NetError`, and every `?` is refused with no fix (B-2026-11-11-25). The
  `.kara` declares `NetError`.
- `String::from_utf8_lossy(..)`: no such function (B-2026-11-11-24);
  `String.from_utf8(..).unwrap_or(String.new())`, right only because the
  run's payloads are ASCII.
- `proto.as_deref() != Some("MQTT")`: no `as_deref` (B-2026-11-11-26);
  `proto != Some("MQTT".to_string())`.

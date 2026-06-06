import Config

# Endpoint config. Mirrors the bare-WS comparators' server posture: TLS
# 1.2+1.3, single self-signed cert (the shared tests/fixtures/tls fixture,
# copied into priv/), no client auth, loopback bind, ephemeral port.
config :bench, Bench.Endpoint,
  adapter: Phoenix.Endpoint.Cowboy2Adapter,
  url: [host: "localhost"],
  render_errors: [formats: []],
  pubsub_server: Bench.PubSub,
  # Phoenix requires a >=64-byte secret_key_base; value is irrelevant to a
  # bench that never signs sessions/tokens.
  secret_key_base: String.duplicate("kara-ws-idle-holder-bench-secret", 2),
  server: true,
  http: false,
  https: [
    ip: {127, 0, 0, 1},
    # Ephemeral — the actual port is read back via :ranch.get_port/1 and
    # printed as BOUND_PORT for the harness. certfile/keyfile resolve
    # relative to the project root (run_server.sh cd's there).
    port: 0,
    certfile: "priv/cert.pem",
    keyfile: "priv/key.pem",
    versions: [:"tlsv1.2", :"tlsv1.3"]
  ]

config :phoenix, :json_library, Jason

# Keep the VM quiet under load; the comparator emits only BOUND_PORT
# (stdout) + a one-line boot banner (stderr).
config :logger, level: :warning

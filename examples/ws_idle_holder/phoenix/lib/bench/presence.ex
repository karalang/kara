defmodule Bench.Presence do
  @moduledoc false
  # Phoenix Presence — the default real-world Channels companion. Each
  # tracked connection adds a CRDT entry replicated over PubSub; this is
  # the framework overhead the presence-on headline run measures and the
  # PRESENCE=off run subtracts out.
  use Phoenix.Presence,
    otp_app: :bench,
    pubsub_server: Bench.PubSub
end

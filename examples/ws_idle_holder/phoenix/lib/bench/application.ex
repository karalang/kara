defmodule Bench.Application do
  @moduledoc false
  use Application

  @impl true
  def start(_type, _args) do
    children = [
      {Phoenix.PubSub, name: Bench.PubSub},
      Bench.Presence,
      Bench.Endpoint
    ]

    opts = [strategy: :one_for_one, name: Bench.Supervisor]
    result = Supervisor.start_link(children, opts)

    # The harness's --server-bin contract reads `BOUND_PORT=<n>` from
    # stdout to learn the ephemeral port. Supervisor.start_link/2 is
    # synchronous — by the time it returns, the Endpoint's ranch listener
    # is up, so the port is queryable. The Cowboy2 adapter names the HTTPS
    # listener <Endpoint>.HTTPS.
    port = :ranch.get_port(Bench.Endpoint.HTTPS)
    IO.puts("BOUND_PORT=#{port}")

    presence = if Bench.BenchChannel.presence_enabled?(), do: "on", else: "off"
    IO.puts(:stderr, "[phoenix] up on https://127.0.0.1:#{port} (presence #{presence})")

    result
  end
end

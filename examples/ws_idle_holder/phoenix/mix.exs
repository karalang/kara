defmodule Bench.MixProject do
  use Mix.Project

  # Phoenix Channels + Presence comparator for the ws_idle_holder bench.
  # Deliberately minimal: a single channel that joins a room and tracks
  # presence — the real-world Elixir prod config (Discord/Pinterest-tier
  # apps run Channels + Presence, not raw Cowboy). See README.md.
  def project do
    [
      app: :bench,
      version: "0.1.0",
      elixir: "~> 1.15",
      start_permanent: Mix.env() == :prod,
      deps: deps()
    ]
  end

  def application do
    [
      mod: {Bench.Application, []},
      extra_applications: [:logger]
    ]
  end

  defp deps do
    [
      {:phoenix, "~> 1.7.14"},
      {:phoenix_pubsub, "~> 2.1"},
      # Cowboy transport (via the Cowboy2 adapter) — the long-standing
      # Phoenix Channels WS transport and what the high-scale Channels
      # deployments ran. Chosen over Bandit because :ranch.get_port/1
      # gives the bound ephemeral port for the harness BOUND_PORT
      # contract; see README "Transport choice".
      {:plug_cowboy, "~> 2.7"},
      {:jason, "~> 1.4"}
    ]
  end
end

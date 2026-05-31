defmodule ParallaxBench.Application do
  # See https://hexdocs.pm/elixir/Application.html
  # for more information on OTP Applications
  @moduledoc false

  use Application

  @impl true
  def start(_type, _args) do
    # Only print BOUND_PORT=<n> when the endpoint is actually serving —
    # `mix test` boots the app with `server: false`, in which case
    # there's no port to report.
    endpoint_serving? =
      Application.get_env(:parallax_bench, ParallaxBenchWeb.Endpoint, [])
      |> Keyword.get(:server, false)

    bound_port_reporter =
      if endpoint_serving? do
        [{ParallaxBench.BoundPortReporter, ParallaxBenchWeb.Endpoint}]
      else
        []
      end

    children =
      [
        ParallaxBenchWeb.Telemetry,
        {DNSCluster, query: Application.get_env(:parallax_bench, :dns_cluster_query) || :ignore},
        {Phoenix.PubSub, name: ParallaxBench.PubSub},
        # Start to serve requests, typically the last entry
        ParallaxBenchWeb.Endpoint
      ] ++ bound_port_reporter

    # See https://hexdocs.pm/elixir/Supervisor.html
    # for other strategies and supported options
    opts = [strategy: :one_for_one, name: ParallaxBench.Supervisor]
    Supervisor.start_link(children, opts)
  end

  # Tell Phoenix to update the endpoint configuration
  # whenever the application is updated.
  @impl true
  def config_change(changed, _new, removed) do
    ParallaxBenchWeb.Endpoint.config_change(changed, removed)
    :ok
  end
end

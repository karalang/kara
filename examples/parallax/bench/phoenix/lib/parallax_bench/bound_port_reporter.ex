defmodule ParallaxBench.BoundPortReporter do
  @moduledoc """
  Prints `BOUND_PORT=<n>` to stdout after the endpoint binds.

  Mirrors the convention used by the Kāra runtime + every other Parallax
  bench comparator (`rust/`, `go/`, `node/`). `bench.sh`'s
  `launch_and_get_port` helper grep's the first `^BOUND_PORT=` line from
  stdout — same scheme works here.

  Reads the bound port from the endpoint's `:http` config (via
  `Phoenix.Endpoint.server_info/1`, which on Bandit returns the actual
  bound TCP port even when the configured port is 0). We're a separate
  child after the endpoint in the supervision tree so by the time we
  start, the listener is up.
  """

  use GenServer
  require Logger

  def start_link(endpoint_module) do
    GenServer.start_link(__MODULE__, endpoint_module, name: __MODULE__)
  end

  @impl true
  def init(endpoint_module) do
    # The endpoint child started before us, so the listener exists; we
    # still poll briefly in case the listener registration is async on
    # some Bandit versions.
    port = await_bound_port(endpoint_module, 50)

    # Use IO.puts so we go to actual stdout, not Logger which routes
    # through `:logger` and prefixes timestamps in dev.
    IO.puts("BOUND_PORT=#{port}")

    # Force a flush so `bench.sh`'s grep sees the line before any HTTP
    # traffic arrives. Standard IO is line-buffered on a tty but block-
    # buffered when piped to a logfile (which is exactly what `bench.sh`
    # does — `cmd >"$log" 2>&1`).
    :ok = :file.datasync(:standard_io)
    {:ok, %{port: port}}
  rescue
    # If datasync fails (some OTP versions don't expose it for stdio),
    # an explicit IO sync is fine — IO.puts already wrote the byte.
    _ -> {:ok, %{}}
  end

  defp await_bound_port(_endpoint_module, 0), do: 0

  defp await_bound_port(endpoint_module, attempts_left) do
    # `Endpoint.server_info(:http)` is a generated callback on every
    # Phoenix.Endpoint module that delegates to the configured adapter
    # (Bandit, in our case). Returns `{:ok, {addr, port}}` once the
    # listener is bound; the configured port may be 0 to ask the OS to
    # pick one and the return value carries the actual bound port.
    case endpoint_module.server_info(:http) do
      {:ok, {_addr, port}} when is_integer(port) and port > 0 ->
        port

      _ ->
        Process.sleep(20)
        await_bound_port(endpoint_module, attempts_left - 1)
    end
  end
end

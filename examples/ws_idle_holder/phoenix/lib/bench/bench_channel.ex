defmodule Bench.BenchChannel do
  @moduledoc false
  use Phoenix.Channel
  alias Bench.Presence

  @impl true
  def join("room:" <> _rest, _params, socket) do
    # Presence.track must run from inside the channel process after the
    # join completes, so defer it to a handle_info. PRESENCE=off skips the
    # track entirely — the sidebar run that quantifies what the presence
    # layer alone costs per connection.
    if presence_enabled?(), do: send(self(), :after_join)
    {:ok, socket}
  end

  @impl true
  def handle_info(:after_join, socket) do
    # One presence entry per connection. The channel pid is unique, so it
    # is a stable per-conn key — this is the bookkeeping the presence
    # layer does for every joined client in a real deployment (a CRDT
    # entry replicated across the cluster + a metas map).
    {:ok, _ref} =
      Presence.track(socket, inspect(self()), %{
        online_at: System.system_time(:second)
      })

    {:noreply, socket}
  end

  # Read PRESENCE at runtime (not as a compile-time @attribute) so the
  # same compiled artifact serves both the presence-on headline run and
  # the PRESENCE=off sidebar run with no recompile.
  def presence_enabled?, do: System.get_env("PRESENCE", "on") != "off"
end

defmodule Bench.UserSocket do
  @moduledoc false
  use Phoenix.Socket

  # All `room:*` topics route to the bench channel. The harness joins a
  # single shared topic (room:bench) by default.
  channel "room:*", Bench.BenchChannel

  @impl true
  def connect(_params, socket, _connect_info), do: {:ok, socket}

  # Anonymous — no per-socket identity/auth. Returning nil disables the
  # "disconnect all sockets for this id" feature we don't use.
  @impl true
  def id(_socket), do: nil
end

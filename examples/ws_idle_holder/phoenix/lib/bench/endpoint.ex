defmodule Bench.Endpoint do
  @moduledoc false
  use Phoenix.Endpoint, otp_app: :bench

  # The Channels WS transport. `/socket/websocket` is the upgrade path the
  # bench client targets (--ws-path /socket/websocket?vsn=2.0.0).
  #
  # timeout: :infinity — a real Phoenix client sends a heartbeat every
  # ~30s and the server closes the socket after `timeout` (default 60s)
  # without one. For an idle-density measurement the bench holds N
  # connections idle while RSS settles; rather than have the harness drive
  # 250K heartbeat timers, we disable the idle timeout. This is purely a
  # liveness setting — it does not change per-connection memory — so the
  # density number is unaffected. Documented in README "Bench
  # accommodations".
  socket "/socket", Bench.UserSocket,
    websocket: [
      timeout: :infinity,
      compress: false,
      max_frame_size: 65_536
    ],
    longpoll: false
end

defmodule ParallaxBenchWeb.Endpoint do
  use Phoenix.Endpoint, otp_app: :parallax_bench

  # Pared down from `mix phx.new`'s default pipeline to match what an
  # API-only Phoenix deployment would carry:
  #
  #   - Plug.Static removed — no static assets for a JSON API.
  #   - Phoenix.CodeReloader removed — bench runs under `MIX_ENV=prod`,
  #     and the comparator is benchmarked, not edited live.
  #   - Plug.Session / Plug.MethodOverride / Plug.Head removed — no
  #     cookies, no HTML form overrides, no HEAD/OPTIONS specials for
  #     this API.
  #   - Plug.Parsers kept — Plug.Conn's JSON-decoder pipeline that any
  #     real Phoenix API uses, even though the bench is GET-only and
  #     doesn't actually parse bodies. Keeping it is the faithful
  #     "what does the typical Phoenix API stack cost" measurement.
  #   - Plug.RequestId + Plug.Telemetry kept — every real Phoenix app
  #     ships with these.
  #
  # Net: bench measures what an Elixir-shop's idiomatic Phoenix API
  # server pays per request, minus the obviously-irrelevant static-
  # file + session plumbing.

  plug Plug.RequestId
  plug Plug.Telemetry, event_prefix: [:phoenix, :endpoint]

  plug Plug.Parsers,
    parsers: [:urlencoded, :multipart, :json],
    pass: ["*/*"],
    json_decoder: Phoenix.json_library()

  plug ParallaxBenchWeb.Router
end

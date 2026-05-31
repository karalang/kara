defmodule ParallaxBenchWeb.DashboardController do
  @moduledoc false

  use ParallaxBenchWeb, :controller

  alias ParallaxBench.Providers

  # `GET /dashboard/:user_id` — same wire shape as the other comparators.
  # The bench's wrk URL is hard-coded to `/dashboard/1`; user_id only
  # feeds the busy-loop addend, so load is user_id-invariant.
  def show(conn, %{"user_id" => user_id_param}) do
    user_id =
      case Integer.parse(user_id_param) do
        {n, _} -> n
        :error -> 1
      end

    dashboard = Providers.get_dashboard(user_id)
    json(conn, dashboard)
  end
end

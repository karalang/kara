defmodule ParallaxBenchWeb.Router do
  use ParallaxBenchWeb, :router

  pipeline :api do
    plug :accepts, ["json"]
  end

  scope "/", ParallaxBenchWeb do
    pipe_through :api

    get "/dashboard/:user_id", DashboardController, :show
  end
end

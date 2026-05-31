import Config

# We don't run a server during test. If one is required,
# you can enable the server option below.
config :parallax_bench, ParallaxBenchWeb.Endpoint,
  http: [ip: {127, 0, 0, 1}, port: 4002],
  secret_key_base: "88TUxRSv4LvxEsm9tsI1KbVN1W1HRp/BO3WDcArzMAqKAaFgkWKAYtvebd/vtmBd",
  server: false

# Print only warnings and errors during test
config :logger, level: :warning

# Initialize plugs at runtime for faster test compilation
config :phoenix, :plug_init_mode, :runtime

# Sort query params output of verified routes for robust url comparisons
config :phoenix,
  sort_verified_routes_query_params: true

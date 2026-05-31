# Phoenix/Elixir reference impl for the Parallax bench.
#
# Mirrors the busy-loop shape used by the Kāra / Rust / Go / Node impls
# so the four (now five) impls stay apples-to-apples. Same hash-mix
# kernel `x = (x*31 + i) mod 1_073_741_789` over the same iteration
# counts (700K / 4M / 1.7M / 2.7M), each fetch returning the busy_loop
# output so the BEAM's JIT can't elide the work.
#
# Sleep substitute (F5). Kāra v1 has no `sleep_ms`; all impls use
# CPU-bound busy loops at iteration counts that approximate the
# 2 / 5 / 8 / 12 ms latency envelope on a modern x86-64 / Apple-Silicon
# core. See `../README.md` for the deviation note.

defmodule ParallaxBench.Providers do
  @moduledoc false

  @fetch_profile_work 700_000
  @fetch_orders_work 4_000_000
  @fetch_notifs_work 1_700_000
  @fetch_recommend_work 2_700_000

  @prime 1_073_741_789

  # Hash-mix kernel — no algebraic identity for `(x*31 + i) mod p`,
  # so neither the BEAM's JIT (BeamAsm) nor a future native compiler
  # can pattern-match this to closed form. Same kernel + constants as
  # the other impls.
  @spec busy_loop(integer()) :: integer()
  def busy_loop(n) when is_integer(n) and n >= 0 do
    busy_loop_step(n, 0, 1)
  end

  defp busy_loop_step(n, i, x) when i < n do
    busy_loop_step(n, i + 1, rem(x * 31 + i, @prime))
  end

  defp busy_loop_step(_n, _i, x), do: x

  @spec fetch_profile_name(integer()) :: binary()
  def fetch_profile_name(user_id) do
    _ = busy_loop(@fetch_profile_work + user_id)
    "Alice"
  end

  @spec fetch_latest_order_id(integer()) :: integer()
  def fetch_latest_order_id(user_id) do
    busy_loop(@fetch_orders_work + user_id)
  end

  @spec fetch_top_notification_kind(integer()) :: integer()
  def fetch_top_notification_kind(user_id) do
    busy_loop(@fetch_notifs_work + user_id)
  end

  @spec fetch_top_recommendation_id(integer()) :: integer()
  def fetch_top_recommendation_id(user_id) do
    busy_loop(@fetch_recommend_work + user_id)
  end

  # Fan-out + join. `Task.async/1` spawns a BEAM process per branch;
  # `Task.await/2` joins. This is the natural Phoenix/OTP idiom — every
  # fan-out branch is its own actor with its own scheduler slice. The
  # developer wires the four spawns by hand (vs. Kāra's auto-par which
  # infers the fan-out from disjoint `reads(R_i)` effects).
  #
  # Timeout is generous (15s) — under heavy load some branches can
  # queue behind the BEAM scheduler past the default 5s; mirrors the
  # tokio / Go behavior of "don't kill the request on slow fan-out".
  @spec get_dashboard(integer()) :: map()
  def get_dashboard(user_id) do
    profile_task = Task.async(fn -> fetch_profile_name(user_id) end)
    order_task = Task.async(fn -> fetch_latest_order_id(user_id) end)
    notif_task = Task.async(fn -> fetch_top_notification_kind(user_id) end)
    recommend_task = Task.async(fn -> fetch_top_recommendation_id(user_id) end)

    profile_name = Task.await(profile_task, 15_000)
    order_id = Task.await(order_task, 15_000)
    notif_kind = Task.await(notif_task, 15_000)
    rec_id = Task.await(recommend_task, 15_000)

    %{
      profile: %{user_id: user_id, name: profile_name},
      latest_order: %{order_id: order_id},
      top_notification: %{kind: notif_kind},
      top_recommendation: %{item_id: rec_id}
    }
  end
end

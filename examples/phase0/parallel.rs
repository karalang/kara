// examples/phase0/parallel.rs
//
// What the Kāra compiler WOULD generate from dashboard.kara.
//
// The compiler analyzed the effect annotations and determined:
//   fetch_profile  → reads(UserDB)
//   fetch_orders   → reads(OrderDB)
//   fetch_notifications → reads(NotifDB)
//
// Three reads on three DIFFERENT resources → no conflicts → safe to parallelize.
// The compiler spawns OS threads (v1 runtime) and joins before the sync point.
//
// Same code, same output — just faster.

use std::thread;
use std::time::{Duration, Instant};

// --- Domain types (identical to sequential version) ---

struct Profile {
    name: String,
    email: String,
    tier: String,
}

struct Order {
    id: u64,
    total: f64,
    status: String,
}

struct Notification {
    message: String,
    unread: bool,
}

struct Dashboard {
    greeting: String,
    order_summary: String,
    alert: String,
}

// --- Simulated database fetches (each takes ~200ms) ---

fn fetch_profile(user_id: u64) -> Result<Profile, String> {
    thread::sleep(Duration::from_millis(200));
    Ok(Profile {
        name: "Alice Chen".into(),
        email: "alice@example.com".into(),
        tier: "Premium".into(),
    })
}

fn fetch_orders(user_id: u64) -> Result<Vec<Order>, String> {
    thread::sleep(Duration::from_millis(200));
    Ok(vec![
        Order { id: 1001, total: 59.99, status: "shipped".into() },
        Order { id: 1002, total: 124.50, status: "pending".into() },
        Order { id: 1003, total: 34.00, status: "delivered".into() },
    ])
}

fn fetch_notifications(user_id: u64) -> Result<Vec<Notification>, String> {
    thread::sleep(Duration::from_millis(200));
    Ok(vec![
        Notification { message: "Order #1001 shipped".into(), unread: true },
        Notification { message: "Welcome to Premium!".into(), unread: true },
        Notification { message: "Order #1003 delivered".into(), unread: false },
    ])
}

fn build_dashboard(profile: Profile, orders: Vec<Order>, notifs: Vec<Notification>) -> Dashboard {
    let total_spent: f64 = orders.iter().map(|o| o.total).sum();
    let pending = orders.iter().filter(|o| o.status == "pending").count();
    let unread = notifs.iter().filter(|n| n.unread).count();

    Dashboard {
        greeting: format!("Welcome back, {} ({})", profile.name, profile.tier),
        order_summary: format!("{} orders (${:.2}), {} pending", orders.len(), total_spent, pending),
        alert: match unread {
            0 => "No new notifications".into(),
            1 => "1 unread notification".into(),
            n => format!("{} unread notifications", n),
        },
    }
}

fn main() {
    let start = Instant::now();

    // ─── COMPILER-GENERATED PARALLEL REGION ───────────────────────
    //
    // The Kāra compiler detected three independent statements:
    //   let profile = fetch_profile(user_id)?;    // reads(UserDB)
    //   let orders  = fetch_orders(user_id)?;      // reads(OrderDB)
    //   let notifs  = fetch_notifications(user_id)?; // reads(NotifDB)
    //
    // Effect analysis:
    //   reads(UserDB) ∥ reads(OrderDB)  → different resources → SAFE
    //   reads(UserDB) ∥ reads(NotifDB)  → different resources → SAFE
    //   reads(OrderDB) ∥ reads(NotifDB) → different resources → SAFE
    //
    // Data dependency analysis:
    //   profile, orders, notifs are independent — no cross-references.
    //
    // Decision: PARALLELIZE all three. Insert sync point before
    //   build_dashboard(profile, orders, notifs)
    // which consumes all three results.
    //
    // Total time ≈ max(200ms, 200ms, 200ms) = ~200ms (vs ~600ms sequential)
    // ──────────────────────────────────────────────────────────────

    let h_profile = thread::spawn(move || fetch_profile(1));
    let h_orders = thread::spawn(move || fetch_orders(1));
    let h_notifs = thread::spawn(move || fetch_notifications(1));

    // Sync point: join all threads, propagate first error (source order).
    let profile = h_profile.join().unwrap().unwrap();
    let orders = h_orders.join().unwrap().unwrap();
    let notifs = h_notifs.join().unwrap().unwrap();

    // ─── END PARALLEL REGION ─────────────────────────────────────

    let dashboard = build_dashboard(profile, orders, notifs);

    let elapsed = start.elapsed();

    println!("{}", dashboard.greeting);
    println!("{}", dashboard.order_summary);
    println!("{}", dashboard.alert);
    eprintln!("[parallel]   completed in {:.0?}", elapsed);
}

// examples/phase0/sequential.rs
//
// Hand-translated sequential version of dashboard.kara.
// This is what a naive translation would produce — no concurrency.
// Each database fetch blocks before the next one starts.

use std::thread;
use std::time::{Duration, Instant};

// --- Domain types (same as Kāra structs) ---

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
    thread::sleep(Duration::from_millis(200)); // simulate I/O
    Ok(Profile {
        name: "Alice Chen".into(),
        email: "alice@example.com".into(),
        tier: "Premium".into(),
    })
}

fn fetch_orders(user_id: u64) -> Result<Vec<Order>, String> {
    thread::sleep(Duration::from_millis(200)); // simulate I/O
    Ok(vec![
        Order { id: 1001, total: 59.99, status: "shipped".into() },
        Order { id: 1002, total: 124.50, status: "pending".into() },
        Order { id: 1003, total: 34.00, status: "delivered".into() },
    ])
}

fn fetch_notifications(user_id: u64) -> Result<Vec<Notification>, String> {
    thread::sleep(Duration::from_millis(200)); // simulate I/O
    Ok(vec![
        Notification { message: "Order #1001 shipped".into(), unread: true },
        Notification { message: "Welcome to Premium!".into(), unread: true },
        Notification { message: "Order #1003 delivered".into(), unread: false },
    ])
}

// --- Pure computation (no I/O) ---

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

    // Sequential: each fetch blocks before the next starts.
    // Total time ≈ 200ms + 200ms + 200ms = ~600ms
    let profile = fetch_profile(1).unwrap();
    let orders = fetch_orders(1).unwrap();
    let notifs = fetch_notifications(1).unwrap();

    let dashboard = build_dashboard(profile, orders, notifs);

    let elapsed = start.elapsed();

    println!("{}", dashboard.greeting);
    println!("{}", dashboard.order_summary);
    println!("{}", dashboard.alert);
    eprintln!("[sequential] completed in {:.0?}", elapsed);
}

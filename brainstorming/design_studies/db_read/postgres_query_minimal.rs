// design_studies/db_read/postgres_query_minimal.rs
//
// Connect to Postgres and print rows from a `users` table.
// Minimal-shaped Rust variant — synchronous, Box<dyn Error>, no pool.
// Good for quick scripts; `postgres_query_production.rs` in the same
// directory shows the async + sqlx + anyhow shape you'd actually ship.
//
// Uses the synchronous `postgres` crate (Cargo.toml: postgres = "0.19").

use std::env;
use std::error::Error;

use postgres::{Client, NoTls};

fn main() -> Result<(), Box<dyn Error>> {
    let url = env::var("DATABASE_URL")?;
    let mut client = Client::connect(&url, NoTls)?;

    let rows = client.query(
        "SELECT id, name, email FROM users ORDER BY id",
        &[],
    )?;

    for row in rows {
        let id: i64 = row.get("id");
        let name: &str = row.get("name");
        let email: &str = row.get("email");
        println!("{id}\t{name}\t{email}");
    }

    Ok(())
}

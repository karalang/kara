// design_studies/db_read/postgres_query_production.rs
//
// Connect to Postgres and print rows from a `users` table.
// Production-shaped Rust variant — async, sqlx, anyhow, pool.
// This is what a Rust engineer would actually ship for a CLI like this;
// `postgres_query_minimal.rs` in the same directory is the minimal
// sync shape, better for quick scripts.
//
// Cargo.toml dependencies:
//   anyhow = "1"
//   sqlx   = { version = "0.8", features = ["runtime-tokio", "postgres"] }
//   tokio  = { version = "1", features = ["macros", "rt-multi-thread"] }

use anyhow::{Context, Result};
use sqlx::FromRow;
use sqlx::postgres::PgPoolOptions;

#[derive(FromRow, Debug)]
struct User {
    id: i64,
    name: String,
    email: String,
}

#[tokio::main]
async fn main() -> Result<()> {
    let url = std::env::var("DATABASE_URL")
        .context("DATABASE_URL must be set")?;

    let pool = PgPoolOptions::new()
        .max_connections(5)
        .connect(&url)
        .await
        .context("failed to connect to Postgres")?;

    let users: Vec<User> = sqlx::query_as(
        "SELECT id, name, email FROM users ORDER BY id",
    )
    .fetch_all(&pool)
    .await
    .context("failed to query users")?;

    for u in users {
        println!("{}\t{}\t{}", u.id, u.name, u.email);
    }

    Ok(())
}

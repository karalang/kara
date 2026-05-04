// design_studies/json_read/read_users.rs
//
// Read a JSON array of users from disk and print rows.
// Usage: cargo run -- <path>
//
// Cargo.toml dependencies:
//   anyhow     = "1"
//   serde      = { version = "1", features = ["derive"] }
//   serde_json = "1"

use std::env;
use std::fs::File;
use std::io::BufReader;

use anyhow::{Context, Result};
use serde::Deserialize;

#[derive(Deserialize, Debug)]
struct User {
    id: i64,
    name: String,
    email: String,
}

fn main() -> Result<()> {
    let path = env::args()
        .nth(1)
        .context("usage: read_users <path>")?;

    let file = File::open(&path)
        .with_context(|| format!("opening {path}"))?;

    let users: Vec<User> = serde_json::from_reader(BufReader::new(file))
        .context("parsing JSON")?;

    for u in users {
        println!("{}\t{}\t{}", u.id, u.name, u.email);
    }

    Ok(())
}

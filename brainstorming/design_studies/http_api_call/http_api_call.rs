// design_studies/http_api_call/http_api_call.rs
//
// GET a JSON endpoint, parse the response, print rows.
// Rust variant — async via tokio + reqwest + serde.
//
// Cargo.toml dependencies:
//   anyhow  = "1"
//   reqwest = { version = "0.12", features = ["json"] }
//   serde   = { version = "1", features = ["derive"] }
//   tokio   = { version = "1", features = ["macros", "rt-multi-thread"] }

use anyhow::{Context, Result};
use serde::Deserialize;

#[derive(Deserialize, Debug)]
struct User {
    id: i64,
    name: String,
    email: String,
}

#[tokio::main]
async fn main() -> Result<()> {
    let url = "https://jsonplaceholder.typicode.com/users";

    let users: Vec<User> = reqwest::get(url)
        .await
        .context("http request failed")?
        .error_for_status()
        .context("http status error")?
        .json()
        .await
        .context("json parse failed")?;

    for u in users {
        println!("{}\t{}\t{}", u.id, u.name, u.email);
    }

    Ok(())
}

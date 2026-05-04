// design_studies/parallel_fanout/parallel_fanout.rs
//
// Fetch N user records concurrently and print aggregated output.
// Rust variant — tokio + reqwest + futures::try_join_all.
//
// Cargo.toml dependencies:
//   anyhow  = "1"
//   futures = "0.3"
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

async fn fetch(client: &reqwest::Client, id: i64) -> Result<User> {
    let url = format!("https://jsonplaceholder.typicode.com/users/{id}");
    client
        .get(&url)
        .send()
        .await
        .with_context(|| format!("http request for id={id} failed"))?
        .error_for_status()
        .with_context(|| format!("http status error for id={id}"))?
        .json()
        .await
        .with_context(|| format!("json parse for id={id}"))
}

#[tokio::main]
async fn main() -> Result<()> {
    let client = reqwest::Client::new();
    let ids = [1, 2, 3, 4, 5];

    let fetches = ids.iter().map(|&id| fetch(&client, id));
    let users = futures::future::try_join_all(fetches).await?;

    for u in users {
        println!("{}\t{}\t{}", u.id, u.name, u.email);
    }

    Ok(())
}

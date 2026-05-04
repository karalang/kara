// design_studies/event_stream/event_stream.rs
//
// Read JSON events line-by-line from stdin and print a one-line
// summary for each. Unbounded push-model source — runs until EOF.
//
// Input shape (one per line):
//   {"event": "login", "user": "alice"}
//
// Cargo.toml dependencies:
//   serde      = { version = "1", features = ["derive"] }
//   serde_json = "1"

use std::io::{self, BufRead, Write};

use serde::Deserialize;

#[derive(Deserialize)]
struct Event {
    event: String,
    user: String,
}

fn main() -> io::Result<()> {
    let stdin = io::stdin();
    let stdout = io::stdout();
    let mut out = stdout.lock();
    let mut err = io::stderr().lock();

    for line in stdin.lock().lines() {
        let line = line?;
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }

        match serde_json::from_str::<Event>(trimmed) {
            Ok(e) => writeln!(out, "[{}] {}", e.event, e.user)?,
            Err(_) => writeln!(err, "bad event: {trimmed}")?,
        }
    }

    Ok(())
}

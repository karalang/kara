//! `kara-lsp` binary entry point (roadmap Track 3).
//!
//! Thin stdio wrapper: the editor launches this process and speaks LSP over
//! its stdin/stdout. All protocol logic lives in the library crate
//! ([`kara_lsp::serve`]) so it stays driveable in-process by the integration
//! tests. `stderr` is kept free for logging.

use std::error::Error;

use lsp_server::Connection;

fn main() -> Result<(), Box<dyn Error + Sync + Send>> {
    eprintln!("kara-lsp: starting (stdio transport)");
    let (connection, io_threads) = Connection::stdio();
    kara_lsp::serve(connection)?;
    io_threads.join()?;
    eprintln!("kara-lsp: shut down cleanly");
    Ok(())
}

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
    let code = kara_lsp::serve_until_exit(connection)?;
    io_threads.join()?;
    if code == 0 {
        eprintln!("kara-lsp: shut down cleanly");
    } else {
        eprintln!("kara-lsp: exit without shutdown");
    }
    std::process::exit(code)
}

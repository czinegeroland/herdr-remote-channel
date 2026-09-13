//! The in-memory reference adapter, as a separate executable.
//!
//! PRD section 21 specifies adapters as separate executables speaking
//! JSON-RPC 2.0 over standard input and output. This is that shape at its
//! smallest: construct a [`MemoryTransport`] and hand it to
//! [`hrc_transport::jsonrpc::serve`].
//!
//! It exists so the conformance suite can run against a real process rather
//! than an in-process fake. An adapter binding tested only in-process proves
//! that the types round-trip, not that the protocol does.
//!
//! State lives in this process's memory, so it lasts exactly as long as the
//! adapter does. That is the point: the reference adapter is for exercising
//! the protocol, not for hosting a channel anyone depends on.

use hrc_transport::jsonrpc;
use hrc_transport::memory::MemoryTransport;

fn main() -> std::process::ExitCode {
    let mut transport = MemoryTransport::new("reference");

    match jsonrpc::serve(&mut transport, std::io::stdin(), std::io::stdout()) {
        Ok(()) => std::process::ExitCode::SUCCESS,
        Err(error) => {
            // Diagnostics go to stderr; stdout carries framed protocol
            // traffic and nothing else.
            eprintln!("hrc-reference-adapter: {error}");
            std::process::ExitCode::FAILURE
        }
    }
}

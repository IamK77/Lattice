//! The daemon: the core running as a persistent process, frontends (in any
//! language) connecting over a Unix socket. See clients/ink/README.md.

#[cfg(unix)]
mod bindings;
#[cfg(unix)]
mod history;
pub mod protocol;

#[cfg(unix)]
pub mod server;

pub use protocol::{ClientMessage, ServerMessage};
#[cfg(unix)]
pub use server::{Daemon, DEFAULT_TEMPLATE};

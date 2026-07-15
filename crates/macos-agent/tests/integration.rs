// Consolidated integration target: one linked binary with focused adapter modules.

#[path = "integration/backend.rs"]
mod backend;
#[path = "integration/cli_contract.rs"]
mod cli_contract;
#[path = "integration/common.rs"]
pub mod common;
#[path = "integration/journal.rs"]
mod journal;
#[path = "integration/mcp.rs"]
mod mcp;
#[path = "integration/peekaboo_contract.rs"]
mod peekaboo_contract;
#[path = "integration/transport.rs"]
mod transport;

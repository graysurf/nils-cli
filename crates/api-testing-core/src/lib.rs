//! Shared core primitives for the api-testing CLIs.

pub mod auth_env;
pub mod cli_contract;
pub mod cli_endpoint;
pub mod cli_history;
pub mod cli_io;
pub mod cli_report;
pub mod cli_util;
pub mod cmd_snippet;
pub mod config;
pub mod env_file;
pub mod graphql;
pub mod grpc;
pub mod history;
pub mod http;
pub mod jq;
pub mod jwt;
pub mod markdown;
pub mod redact;
pub mod report;
pub mod rest;
pub mod suite;
pub mod websocket;

pub type Result<T> = std::result::Result<T, anyhow::Error>;

pub const VERSION: &str = env!("CARGO_PKG_VERSION");

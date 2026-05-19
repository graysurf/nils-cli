// Consolidated integration test target.
// Each former `tests/*.rs` is declared as a submodule here so the crate
// links one integration test binary instead of many. This keeps the
// dev-loop link phase O(crates) instead of O(test-files).

#[path = "integration/agent_commit.rs"]
mod agent_commit;
#[path = "integration/agent_commit_fallback.rs"]
mod agent_commit_fallback;
#[path = "integration/agent_exec.rs"]
mod agent_exec;
#[path = "integration/agent_prompt.rs"]
mod agent_prompt;
#[path = "integration/agent_templates.rs"]
mod agent_templates;
#[path = "integration/auth_auto_refresh.rs"]
mod auth_auto_refresh;
#[path = "integration/auth_current_sync.rs"]
mod auth_current_sync;
#[path = "integration/auth_json_contract.rs"]
mod auth_json_contract;
#[path = "integration/auth_json_contract_more.rs"]
mod auth_json_contract_more;
#[path = "integration/auth_login.rs"]
mod auth_login;
#[path = "integration/auth_refresh.rs"]
mod auth_refresh;
#[path = "integration/auth_remove.rs"]
mod auth_remove;
#[path = "integration/auth_save.rs"]
mod auth_save;
#[path = "integration/auth_use.rs"]
mod auth_use;
#[path = "integration/completion_contract.rs"]
mod completion_contract;
#[path = "integration/completion_smoke.rs"]
mod completion_smoke;
#[path = "integration/config.rs"]
mod config;
#[path = "integration/diag_json_contract.rs"]
mod diag_json_contract;
#[path = "integration/dispatch.rs"]
mod dispatch;
#[path = "integration/exit_codes.rs"]
mod exit_codes;
#[path = "integration/fs.rs"]
mod fs;
#[path = "integration/help_snapshot.rs"]
mod help_snapshot;
#[path = "integration/json.rs"]
mod json;
#[path = "integration/jwt.rs"]
mod jwt;
#[path = "integration/main_entrypoint.rs"]
mod main_entrypoint;
#[path = "integration/parity_oracle.rs"]
mod parity_oracle;
#[path = "integration/paths.rs"]
mod paths;
#[path = "integration/prompt_segment_cached.rs"]
mod prompt_segment_cached;
#[path = "integration/prompt_segment_refresh.rs"]
mod prompt_segment_refresh;
#[path = "integration/prompts.rs"]
mod prompts;
#[path = "integration/rate_limits_all.rs"]
mod rate_limits_all;
#[path = "integration/rate_limits_ansi.rs"]
mod rate_limits_ansi;
#[path = "integration/rate_limits_async.rs"]
mod rate_limits_async;
#[path = "integration/rate_limits_client.rs"]
mod rate_limits_client;
#[path = "integration/rate_limits_client_more.rs"]
mod rate_limits_client_more;
#[path = "integration/rate_limits_network.rs"]
mod rate_limits_network;
#[path = "integration/rate_limits_render.rs"]
mod rate_limits_render;
#[path = "integration/rate_limits_single.rs"]
mod rate_limits_single;
#[path = "integration/runtime_auth_contract.rs"]
mod runtime_auth_contract;
#[path = "integration/runtime_error_contract.rs"]
mod runtime_error_contract;
#[path = "integration/runtime_exec_contract.rs"]
mod runtime_exec_contract;
#[path = "integration/runtime_paths_config_contract.rs"]
mod runtime_paths_config_contract;

// Consolidated integration test target.
// Each former `tests/*.rs` is declared as a submodule here so the crate
// links one integration test binary instead of many. This keeps the
// dev-loop link phase O(crates) instead of O(test-files).

#[path = "integration/cli_contract.rs"]
mod cli_contract;
#[path = "integration/markdown_table_canonicalization.rs"]
mod markdown_table_canonicalization;
#[path = "integration/provider_runtime_contract.rs"]
mod provider_runtime_contract;

use codex_cli::runtime::{CoreError, CoreErrorCategory, ProviderCategoryHint};
use pretty_assertions::assert_eq;

#[test]
fn runtime_error_contract_preserves_category_code_and_retryable() {
    let error = CoreError::validation("invalid-input", "input is invalid").with_retryable(false);

    assert_eq!(error.category, CoreErrorCategory::Validation);
    assert_eq!(error.code, "invalid-input");
    assert_eq!(error.message, "input is invalid");
    assert!(!error.retryable);
}

#[test]
fn runtime_error_contract_exit_code_hints_are_stable() {
    assert_eq!(CoreError::config("bad-config", "x").exit_code_hint(), 64);
    assert_eq!(CoreError::validation("bad-input", "x").exit_code_hint(), 64);
    assert_eq!(CoreError::auth("bad-auth", "x").exit_code_hint(), 1);
    assert_eq!(CoreError::exec("exec-failed", "x").exit_code_hint(), 1);
    assert_eq!(
        CoreError::dependency("missing-binary", "x").exit_code_hint(),
        1
    );
    assert_eq!(CoreError::internal("unexpected", "x").exit_code_hint(), 1);
}

#[test]
fn runtime_error_contract_provider_category_hints_are_stable() {
    assert_eq!(
        CoreError::auth("invalid-auth-file", "x").provider_category_hint(),
        ProviderCategoryHint::Auth
    );
    assert_eq!(
        CoreError::dependency("missing-binary", "x").provider_category_hint(),
        ProviderCategoryHint::Dependency
    );
    assert_eq!(
        CoreError::validation("bad-input", "x").provider_category_hint(),
        ProviderCategoryHint::Validation
    );
    assert_eq!(
        CoreError::config("bad-config", "x").provider_category_hint(),
        ProviderCategoryHint::Validation
    );
    assert_eq!(
        CoreError::exec("exec-failed", "x").provider_category_hint(),
        ProviderCategoryHint::Internal
    );
    assert_eq!(
        CoreError::internal("unknown", "x").provider_category_hint(),
        ProviderCategoryHint::Internal
    );
}

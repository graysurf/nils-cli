use gemini_cli::agent::exec;
use nils_test_support::{EnvGuard, GlobalStateLock};

#[test]
fn exec_dangerous_missing_prompt_exits_1() {
    let mut stderr: Vec<u8> = Vec::new();
    let code = exec::exec_dangerous("", "caller", &mut stderr);
    assert_eq!(code, 1);
    assert!(String::from_utf8_lossy(&stderr).contains("_gemini_exec_dangerous: missing prompt"));
}

#[test]
fn require_allow_dangerous_without_caller_uses_gemini_prefix() {
    let lock = GlobalStateLock::new();
    let _danger = EnvGuard::set(&lock, "GEMINI_ALLOW_DANGEROUS_ENABLED", "false");

    let mut stderr: Vec<u8> = Vec::new();
    let allowed = exec::require_allow_dangerous(None, &mut stderr);
    assert!(!allowed);
    assert!(
        String::from_utf8_lossy(&stderr)
            .contains("gemini: disabled (set GEMINI_ALLOW_DANGEROUS_ENABLED=true)")
    );
}

#[test]
fn require_allow_dangerous_warns_on_invalid_value() {
    let lock = GlobalStateLock::new();
    let _danger = EnvGuard::set(&lock, "GEMINI_ALLOW_DANGEROUS_ENABLED", "wat");

    let mut stderr: Vec<u8> = Vec::new();
    let allowed = exec::require_allow_dangerous(Some("caller"), &mut stderr);
    assert!(!allowed);
    let err = String::from_utf8_lossy(&stderr);
    assert!(err.contains("warning: GEMINI_ALLOW_DANGEROUS_ENABLED must be true|false"));
    assert!(err.contains("caller: disabled"));
}

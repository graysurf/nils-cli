use codex_cli::agent::exec;
use nils_test_support::{EnvGuard, GlobalStateLock};
use pretty_assertions::assert_eq;

#[test]
fn exec_dangerous_missing_prompt_exits_1() {
    let mut stderr: Vec<u8> = Vec::new();
    let code = exec::exec_dangerous("", "caller", &mut stderr);
    assert_eq!(code, 1);
    assert!(String::from_utf8_lossy(&stderr).contains("_codex_exec_dangerous: missing prompt"));
}

#[test]
fn require_allow_dangerous_without_caller_uses_codex_prefix() {
    let lock = GlobalStateLock::new();
    let _danger = EnvGuard::set(&lock, "CODEX_ALLOW_DANGEROUS_ENABLED", "false");

    let mut stderr: Vec<u8> = Vec::new();
    let allowed = exec::require_allow_dangerous(None, &mut stderr);
    assert!(!allowed);
    assert!(
        String::from_utf8_lossy(&stderr)
            .contains("codex: disabled (set CODEX_ALLOW_DANGEROUS_ENABLED=true)")
    );
}

#[test]
fn require_allow_dangerous_treats_empty_as_false() {
    let lock = GlobalStateLock::new();
    let _danger = EnvGuard::set(&lock, "CODEX_ALLOW_DANGEROUS_ENABLED", "");

    let mut stderr: Vec<u8> = Vec::new();
    let allowed = exec::require_allow_dangerous(Some("caller"), &mut stderr);
    assert!(!allowed);
    assert!(String::from_utf8_lossy(&stderr).contains("caller: disabled"));
}

#[test]
fn exec_dangerous_returns_1_when_codex_missing() {
    let lock = GlobalStateLock::new();
    let _danger = EnvGuard::set(&lock, "CODEX_ALLOW_DANGEROUS_ENABLED", "true");
    let _path = EnvGuard::set(&lock, "PATH", "");

    let mut stderr: Vec<u8> = Vec::new();
    let code = exec::exec_dangerous("hi", "caller", &mut stderr);
    assert_eq!(code, 1);
    assert!(String::from_utf8_lossy(&stderr).contains("failed to run codex exec"));
}

#[test]
fn require_allow_dangerous_warns_on_invalid_value() {
    let lock = GlobalStateLock::new();
    let _danger = EnvGuard::set(&lock, "CODEX_ALLOW_DANGEROUS_ENABLED", "wat");

    let mut stderr: Vec<u8> = Vec::new();
    let allowed = exec::require_allow_dangerous(Some("caller"), &mut stderr);
    assert!(!allowed);
    let err = String::from_utf8_lossy(&stderr);
    assert!(err.contains("warning: CODEX_ALLOW_DANGEROUS_ENABLED must be true|false"));
    assert!(err.contains("caller: disabled"));
}

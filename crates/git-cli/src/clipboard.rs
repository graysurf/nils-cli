use anyhow::Result;
use nils_common::clipboard::{ClipboardOutcome, ClipboardPolicy, ClipboardTool, copy_best_effort};
use std::env;

const CLIPBOARD_TOOL_ORDER: [ClipboardTool; 4] = [
    ClipboardTool::Pbcopy,
    ClipboardTool::WlCopy,
    ClipboardTool::Xclip,
    ClipboardTool::Xsel,
];

pub fn set_clipboard_best_effort(text: &str) -> Result<()> {
    if env::var("GIT_CLI_FIXTURE_CLIPBOARD_MODE").ok().as_deref() == Some("missing") {
        eprintln!("⚠️  No clipboard tool found (requires pbcopy, wl-copy, xclip, or xsel)");
        return Ok(());
    }

    let policy = ClipboardPolicy::new(&CLIPBOARD_TOOL_ORDER);
    if matches!(
        copy_best_effort(text, &policy),
        ClipboardOutcome::SkippedNoTool | ClipboardOutcome::SkippedFailure
    ) {
        eprintln!("⚠️  No clipboard tool found (requires pbcopy, wl-copy, xclip, or xsel)");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use nils_test_support::{EnvGuard, GlobalStateLock, StubBinDir, prepend_path};
    use pretty_assertions::assert_eq;
    use std::fs;
    use tempfile::TempDir;

    fn write_clipboard_stub(stubs: &StubBinDir, name: &str) {
        stubs.write_exe(
            name,
            &format!(
                r#"#!/bin/bash
set -euo pipefail
chosen="${{CLIPBOARD_TOOL_CHOSEN:?CLIPBOARD_TOOL_CHOSEN is required}}"
payload="${{CLIPBOARD_PAYLOAD_OUT:?CLIPBOARD_PAYLOAD_OUT is required}}"
printf "%s" "{name}" > "$chosen"
/bin/cat > "$payload"
"#
            ),
        );
    }

    #[test]
    fn set_clipboard_best_effort_prefers_pbcopy_when_present() {
        let lock = GlobalStateLock::new();

        let stubs = StubBinDir::new();
        let out_dir = TempDir::new().expect("tempdir");
        let out_path = out_dir.path().join("pbcopy.out");

        stubs.write_exe(
            "pbcopy",
            r#"#!/bin/bash
set -euo pipefail
out="${PB_COPY_OUT:?PB_COPY_OUT is required}"
/bin/cat > "$out"
"#,
        );

        let _path_guard: EnvGuard = prepend_path(&lock, stubs.path());
        let out_path_str = out_path.to_string_lossy();
        let _out_guard = EnvGuard::set(&lock, "PB_COPY_OUT", out_path_str.as_ref());

        set_clipboard_best_effort("hello").expect("copy");
        let out = fs::read_to_string(out_path).expect("read stub output");
        assert_eq!(out, "hello");
    }

    #[test]
    fn set_clipboard_best_effort_prefers_pbcopy_over_other_tools() {
        let lock = GlobalStateLock::new();
        let stubs = StubBinDir::new();
        write_clipboard_stub(&stubs, "pbcopy");
        write_clipboard_stub(&stubs, "wl-copy");
        write_clipboard_stub(&stubs, "xclip");
        write_clipboard_stub(&stubs, "xsel");

        let out_dir = TempDir::new().expect("tempdir");
        let chosen_path = out_dir.path().join("chosen.txt");
        let payload_path = out_dir.path().join("payload.txt");

        let _path_guard = EnvGuard::set(&lock, "PATH", &stubs.path_str());
        let chosen_path_str = chosen_path.to_string_lossy();
        let payload_path_str = payload_path.to_string_lossy();
        let _chosen_guard = EnvGuard::set(&lock, "CLIPBOARD_TOOL_CHOSEN", chosen_path_str.as_ref());
        let _payload_guard =
            EnvGuard::set(&lock, "CLIPBOARD_PAYLOAD_OUT", payload_path_str.as_ref());

        set_clipboard_best_effort("hello").expect("copy");
        assert_eq!(
            fs::read_to_string(chosen_path).expect("chosen"),
            "pbcopy".to_string()
        );
        assert_eq!(fs::read_to_string(payload_path).expect("payload"), "hello");
    }

    #[test]
    fn set_clipboard_best_effort_prefers_wl_copy_over_xclip_and_xsel() {
        let lock = GlobalStateLock::new();
        let stubs = StubBinDir::new();
        write_clipboard_stub(&stubs, "wl-copy");
        write_clipboard_stub(&stubs, "xclip");
        write_clipboard_stub(&stubs, "xsel");

        let out_dir = TempDir::new().expect("tempdir");
        let chosen_path = out_dir.path().join("chosen.txt");
        let payload_path = out_dir.path().join("payload.txt");

        let _path_guard = EnvGuard::set(&lock, "PATH", &stubs.path_str());
        let chosen_path_str = chosen_path.to_string_lossy();
        let payload_path_str = payload_path.to_string_lossy();
        let _chosen_guard = EnvGuard::set(&lock, "CLIPBOARD_TOOL_CHOSEN", chosen_path_str.as_ref());
        let _payload_guard =
            EnvGuard::set(&lock, "CLIPBOARD_PAYLOAD_OUT", payload_path_str.as_ref());

        set_clipboard_best_effort("hello").expect("copy");
        assert_eq!(
            fs::read_to_string(chosen_path).expect("chosen"),
            "wl-copy".to_string()
        );
        assert_eq!(fs::read_to_string(payload_path).expect("payload"), "hello");
    }

    #[test]
    fn set_clipboard_best_effort_prefers_xclip_over_xsel() {
        let lock = GlobalStateLock::new();
        let stubs = StubBinDir::new();
        write_clipboard_stub(&stubs, "xclip");
        write_clipboard_stub(&stubs, "xsel");

        let out_dir = TempDir::new().expect("tempdir");
        let chosen_path = out_dir.path().join("chosen.txt");
        let payload_path = out_dir.path().join("payload.txt");

        let _path_guard = EnvGuard::set(&lock, "PATH", &stubs.path_str());
        let chosen_path_str = chosen_path.to_string_lossy();
        let payload_path_str = payload_path.to_string_lossy();
        let _chosen_guard = EnvGuard::set(&lock, "CLIPBOARD_TOOL_CHOSEN", chosen_path_str.as_ref());
        let _payload_guard =
            EnvGuard::set(&lock, "CLIPBOARD_PAYLOAD_OUT", payload_path_str.as_ref());

        set_clipboard_best_effort("hello").expect("copy");
        assert_eq!(
            fs::read_to_string(chosen_path).expect("chosen"),
            "xclip".to_string()
        );
        assert_eq!(fs::read_to_string(payload_path).expect("payload"), "hello");
    }

    #[test]
    fn set_clipboard_best_effort_uses_xsel_when_present() {
        let lock = GlobalStateLock::new();

        let stubs = StubBinDir::new();
        let out_dir = TempDir::new().expect("tempdir");
        let out_path = out_dir.path().join("xsel.out");

        stubs.write_exe(
            "xsel",
            r#"#!/bin/bash
set -euo pipefail
out="${XSEL_OUT:?XSEL_OUT is required}"
/bin/cat > "$out"
"#,
        );

        let _path_guard = EnvGuard::set(&lock, "PATH", &stubs.path_str());
        let out_path_str = out_path.to_string_lossy();
        let _out_guard = EnvGuard::set(&lock, "XSEL_OUT", out_path_str.as_ref());

        set_clipboard_best_effort("hello").expect("copy");
        let out = fs::read_to_string(out_path).expect("read stub output");
        assert_eq!(out, "hello");
    }

    #[test]
    fn set_clipboard_best_effort_falls_back_when_pbcopy_fails() {
        let lock = GlobalStateLock::new();

        let stubs = StubBinDir::new();
        let out_dir = TempDir::new().expect("tempdir");
        let out_path = out_dir.path().join("wl-copy.out");

        stubs.write_exe(
            "pbcopy",
            r#"#!/bin/bash
exit 1
"#,
        );
        stubs.write_exe(
            "wl-copy",
            r#"#!/bin/bash
set -euo pipefail
out="${WL_COPY_OUT:?WL_COPY_OUT is required}"
/bin/cat > "$out"
"#,
        );

        let _path_guard = EnvGuard::set(&lock, "PATH", &stubs.path_str());
        let out_path_str = out_path.to_string_lossy();
        let _out_guard = EnvGuard::set(&lock, "WL_COPY_OUT", out_path_str.as_ref());

        set_clipboard_best_effort("hello").expect("copy");
        let out = fs::read_to_string(out_path).expect("read stub output");
        assert_eq!(out, "hello");
    }

    #[test]
    fn set_clipboard_best_effort_missing_mode_short_circuits_with_ok() {
        let lock = GlobalStateLock::new();
        let _mode = EnvGuard::set(&lock, "GIT_CLI_FIXTURE_CLIPBOARD_MODE", "missing");
        set_clipboard_best_effort("hello").expect("missing mode should still succeed");
    }
}

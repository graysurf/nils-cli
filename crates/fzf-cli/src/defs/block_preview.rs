use crate::fzf;
use anyhow::{Context, Result};
use nils_common::clipboard::{ClipboardPolicy, ClipboardTool, copy_best_effort};
use std::collections::HashMap;
use std::io::Write;
use std::os::unix::fs::PermissionsExt;

pub struct Block {
    pub header: String,
    pub body: String,
}

const CLIPBOARD_TOOL_ORDER: [ClipboardTool; 3] = [
    ClipboardTool::Pbcopy,
    ClipboardTool::WlCopy,
    ClipboardTool::Xclip,
];

pub fn run(blocks: &[Block], default_query: &str) -> Result<(i32, Option<String>)> {
    let delim = std::env::var("FZF_DEF_DELIM").unwrap_or_default();
    let enddelim = std::env::var("FZF_DEF_DELIM_END").unwrap_or_default();
    if delim.is_empty() || enddelim.is_empty() {
        println!("❌ Error: FZF_DEF_DELIM or FZF_DEF_DELIM_END is not set.");
        println!("💡 Please export FZF_DEF_DELIM and FZF_DEF_DELIM_END before running.");
        return Ok((1, None));
    }

    let mut header_to_block: HashMap<String, String> = HashMap::new();
    let mut tmp = tempfile::NamedTempFile::new().context("mktemp")?;
    for b in blocks {
        let _ = writeln!(tmp, "{delim}");
        let _ = writeln!(tmp, "{}", b.header);
        if !b.body.is_empty() {
            let _ = writeln!(tmp, "{}", b.body);
        }
        let _ = writeln!(tmp, "{enddelim}\n");

        let mut rendered = String::new();
        rendered.push_str(&b.header);
        rendered.push('\n');
        rendered.push('\n');
        rendered.push_str(&b.body);
        if !rendered.ends_with('\n') {
            rendered.push('\n');
        }
        header_to_block.insert(b.header.clone(), rendered);
    }
    tmp.flush().context("flush fzf block data")?;

    let mut preview = tempfile::NamedTempFile::new().context("mktemp preview script")?;
    preview
        .write_all(
            br#"#!/usr/bin/env -S awk -f
BEGIN {
  target      = ENVIRON["FZF_PREVIEW_TARGET"]
  start_delim = ENVIRON["FZF_DEF_DELIM"]
  end_delim   = ENVIRON["FZF_DEF_DELIM_END"]
  printing    = 0
}
{
  if ($0 == start_delim) {
    getline header
    if (header == target) {
      print header
      print ""
      printing = 1
      next
    }
  }
  if (printing && $0 == end_delim) exit
  if (printing) print
}
"#,
        )
        .context("write preview script")?;
    preview.flush().context("flush preview script")?;

    let preview_path = preview.into_temp_path();
    std::fs::set_permissions(&preview_path, std::fs::Permissions::from_mode(0o755))
        .context("make preview script executable")?;

    let input = blocks
        .iter()
        .map(|b| b.header.as_str())
        .collect::<Vec<_>>()
        .join("\n");

    let args_vec: Vec<String> = vec![
        "--ansi".to_string(),
        "--reverse".to_string(),
        "--height=50%".to_string(),
        "--prompt".to_string(),
        "» Select > ".to_string(),
        "--query".to_string(),
        default_query.to_string(),
        "--preview-window=right:70%:wrap".to_string(),
        "--preview".to_string(),
        format!(
            "FZF_PREVIEW_TARGET={{}} {} {}",
            preview_path.to_string_lossy(),
            tmp.path().to_string_lossy()
        ),
    ];

    let args_ref: Vec<&str> = args_vec.iter().map(|s| s.as_str()).collect();
    let (code, selected) = fzf::run_lines(
        &format!("{input}\n"),
        &args_ref,
        &[("FZF_DEF_DELIM", &delim), ("FZF_DEF_DELIM_END", &enddelim)],
    )?;

    if code != 0 {
        return Ok((0, None));
    }

    let Some(sel) = selected.first().cloned() else {
        return Ok((0, None));
    };

    let out = match header_to_block.get(&sel) {
        Some(v) => v.clone(),
        None => return Ok((0, None)),
    };

    let _ = copy_best_effort(&out, &ClipboardPolicy::new(&CLIPBOARD_TOOL_ORDER));
    Ok((0, Some(out)))
}

#[cfg(test)]
mod tests {
    use super::*;
    use nils_test_support::{EnvGuard, GlobalStateLock, StubBinDir, prepend_path, stubs};
    use pretty_assertions::assert_eq;
    use std::fs;
    use tempfile::TempDir;

    #[test]
    fn run_requires_delimiters() {
        let lock = GlobalStateLock::new();
        let _guard = EnvGuard::set(&lock, "FZF_DEF_DELIM", "");
        let _guard_end = EnvGuard::set(&lock, "FZF_DEF_DELIM_END", "");

        let (code, out) = run(&[], "").expect("run");
        assert_eq!(code, 1);
        assert!(out.is_none());
    }

    #[test]
    fn run_renders_and_copies_selected_block() {
        let lock = GlobalStateLock::new();
        let temp = TempDir::new().unwrap();

        let out_dir = temp.path().join("fzf-out");
        fs::create_dir_all(&out_dir).unwrap();
        fs::write(out_dir.join("1.out"), "Header A\n").unwrap();

        let clipboard = temp.path().join("clipboard.txt");
        let stub = StubBinDir::new();
        stub.write_exe("fzf", stubs::fzf_stub_script());
        stub.write_exe(
            "pbcopy",
            r#"#!/bin/bash
set -euo pipefail
cat > "${PBCOPY_OUT:?}"
"#,
        );

        let _guard_path = prepend_path(&lock, stub.path());
        let out_dir_s = out_dir.to_string_lossy().to_string();
        let clipboard_s = clipboard.to_string_lossy().to_string();
        let _guard_out = EnvGuard::set(&lock, "FZF_STUB_OUT_DIR", &out_dir_s);
        let _guard_delim = EnvGuard::set(&lock, "FZF_DEF_DELIM", "---");
        let _guard_end = EnvGuard::set(&lock, "FZF_DEF_DELIM_END", "+++");
        let _guard_clip = EnvGuard::set(&lock, "PBCOPY_OUT", &clipboard_s);

        let blocks = vec![Block {
            header: "Header A".to_string(),
            body: "line1\nline2".to_string(),
        }];
        let (code, out) = run(&blocks, "").expect("run");
        assert_eq!(code, 0);
        let out = out.expect("output");
        assert!(out.contains("Header A"));
        assert!(out.contains("line1"));
        let clipboard_out = fs::read_to_string(&clipboard).unwrap();
        assert_eq!(clipboard_out, out);
    }

    #[test]
    fn run_preview_script_is_executable_by_fzf() {
        let lock = GlobalStateLock::new();
        let temp = TempDir::new().unwrap();

        let clipboard = temp.path().join("clipboard.txt");
        let stub = StubBinDir::new();
        stub.write_exe(
            "fzf",
            r#"#!/bin/bash
set -euo pipefail

preview=''
args=("$@")
for ((i = 0; i < ${#args[@]}; i++)); do
  if [[ "${args[$i]}" == "--preview" ]]; then
    preview="${args[$((i + 1))]}"
    break
  fi
done

selected=''
while IFS= read -r line; do
  selected="$line"
  break
done

if [[ -n "$preview" ]]; then
  FZF_PREVIEW_TARGET="$selected" eval "$preview" >/dev/null
fi

printf '%s\n' "$selected"
"#,
        );
        stub.write_exe(
            "pbcopy",
            r#"#!/bin/bash
set -euo pipefail
cat > "${PBCOPY_OUT:?}"
"#,
        );

        let _guard_path = prepend_path(&lock, stub.path());
        let clipboard_s = clipboard.to_string_lossy().to_string();
        let _guard_delim = EnvGuard::set(&lock, "FZF_DEF_DELIM", "---");
        let _guard_end = EnvGuard::set(&lock, "FZF_DEF_DELIM_END", "+++");
        let _guard_clip = EnvGuard::set(&lock, "PBCOPY_OUT", &clipboard_s);

        let blocks = vec![Block {
            header: "Header A".to_string(),
            body: "line1\nline2".to_string(),
        }];
        let (code, out) = run(&blocks, "").expect("run");
        assert_eq!(code, 0);
        let out = out.expect("output");
        assert!(out.contains("Header A"));
    }
}

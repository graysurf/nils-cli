//! `agent-memory search` — case-insensitive substring search over note content
//! (frontmatter, including the `description`, plus the body) across a scope.

use std::fs;
use std::path::PathBuf;

use serde_json::json;

use nils_common::cli_contract::{OutputFormat, schema_version_for};
use nils_common::fs::display_path;

use crate::cli::SearchArgs;
use crate::{CliError, EXIT_OK, EXIT_RUNTIME, Layout, markdown_files};

struct Hit {
    scope: String,
    file: String,
    line_no: usize,
    text: String,
}

pub(crate) fn run(layout: &Layout, args: &SearchArgs) -> Result<i32, CliError> {
    let needle = args.term.to_lowercase();

    let scopes: Vec<(String, PathBuf)> = if args.all {
        crate::memory_scopes(layout)?
    } else {
        let scope = args.scope.as_deref().unwrap_or("global");
        let dir = layout.resolve_scope(Some(scope))?;
        if !dir.is_dir() {
            return Err(CliError::runtime(format!(
                "not found: {}",
                display_path(&dir)
            )));
        }
        vec![(scope.to_string(), dir)]
    };

    let mut hits = Vec::new();
    for (label, dir) in &scopes {
        for file in markdown_files(dir)? {
            let name = file
                .file_name()
                .map(|value| value.to_string_lossy().into_owned())
                .unwrap_or_default();
            if name == "MEMORY.md" {
                continue;
            }
            let Ok(contents) = fs::read_to_string(&file) else {
                continue;
            };
            for (index, line) in contents.lines().enumerate() {
                if line.to_lowercase().contains(&needle) {
                    hits.push(Hit {
                        scope: label.clone(),
                        file: name.clone(),
                        line_no: index + 1,
                        text: line.trim().to_string(),
                    });
                }
            }
        }
    }

    let format = if args.json {
        OutputFormat::Json
    } else {
        args.format
    };
    if format.is_json() {
        let records: Vec<serde_json::Value> = hits
            .iter()
            .map(|hit| {
                json!({
                    "scope": hit.scope,
                    "file": hit.file,
                    "line": hit.line_no,
                    "text": hit.text,
                })
            })
            .collect();
        let doc = json!({
            "schema_version": schema_version_for("agent-memory", "search", 1),
            "term": args.term,
            "count": hits.len(),
            "hits": records,
        });
        println!(
            "{}",
            serde_json::to_string(&doc).expect("search report should serialize")
        );
    } else {
        for hit in &hits {
            println!("{}/{}:{}: {}", hit.scope, hit.file, hit.line_no, hit.text);
        }
    }

    Ok(if hits.is_empty() {
        EXIT_RUNTIME
    } else {
        EXIT_OK
    })
}

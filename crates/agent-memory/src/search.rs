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

#[cfg(test)]
mod tests {
    use super::*;

    use pretty_assertions::assert_eq;
    use tempfile::TempDir;

    struct Fixture {
        _tmp: TempDir,
        layout: Layout,
        global: PathBuf,
    }

    fn fixture() -> Fixture {
        let tmp = TempDir::new().unwrap();
        let root = tmp.path().join("agent-memory");
        let global = root.join("global");
        fs::create_dir_all(&global).unwrap();
        fs::write(
            global.join("MEMORY.md"),
            "# Memory\n- [deploy](deploy.md) — how the deploy works\n",
        )
        .unwrap();
        fs::write(
            global.join("deploy.md"),
            "---\nname: deploy\ndescription: How the DEPLOY pipeline works\n---\n\nRun the deploy script.\n",
        )
        .unwrap();
        fs::write(
            global.join("other.md"),
            "---\nname: other\ndescription: unrelated\n---\n\nnothing to see\n",
        )
        .unwrap();
        Fixture {
            layout: Layout { root },
            global,
            _tmp: tmp,
        }
    }

    fn args(term: &str) -> SearchArgs {
        SearchArgs {
            term: term.to_string(),
            scope: Some("global".to_string()),
            all: false,
            format: OutputFormat::Text,
            json: false,
        }
    }

    #[test]
    fn a_match_anywhere_in_a_note_is_case_insensitive() {
        let fixture = fixture();

        // The needle appears in the frontmatter description in a different case
        // and in the body in yet another, and both must match.
        assert_eq!(
            run(&fixture.layout, &args("deploy")).expect("search"),
            EXIT_OK
        );
        assert_eq!(
            run(&fixture.layout, &args("DEPLOY")).expect("search"),
            EXIT_OK
        );
        assert_eq!(
            run(&fixture.layout, &args("PiPeLiNe")).expect("search"),
            EXIT_OK
        );
    }

    #[test]
    fn no_match_is_reported_with_a_non_zero_exit() {
        let fixture = fixture();

        assert_eq!(
            run(&fixture.layout, &args("absolutely-not-present")).expect("search"),
            EXIT_RUNTIME
        );
    }

    #[test]
    fn the_index_file_is_never_searched() {
        let fixture = fixture();
        // Only MEMORY.md carries this text, so a hit would mean the index was
        // scanned and every note would appear to match its own index line.
        fs::write(
            fixture.global.join("MEMORY.md"),
            "# Memory\n- [x](x.md) — indexonlyneedle\n",
        )
        .unwrap();

        assert_eq!(
            run(&fixture.layout, &args("indexonlyneedle")).expect("search"),
            EXIT_RUNTIME
        );
    }

    #[test]
    fn an_unreadable_note_is_skipped_rather_than_failing_the_search() {
        let fixture = fixture();
        // A directory named like a note cannot be read as text.
        fs::create_dir(fixture.global.join("broken.md")).unwrap();

        assert_eq!(
            run(&fixture.layout, &args("deploy")).expect("search"),
            EXIT_OK
        );
    }

    #[test]
    fn a_missing_scope_directory_is_a_runtime_error() {
        let fixture = fixture();
        let mut missing = args("deploy");
        missing.scope = Some("agents/absent".to_string());

        let err = run(&fixture.layout, &missing).expect_err("missing scope");

        assert_eq!(err.exit_code, EXIT_RUNTIME);
        assert!(err.message.starts_with("not found: "), "{}", err.message);
    }

    #[test]
    fn json_output_is_selected_by_either_the_flag_or_the_alias() {
        let fixture = fixture();

        let mut alias = args("deploy");
        alias.json = true;
        assert_eq!(run(&fixture.layout, &alias).expect("search"), EXIT_OK);

        let mut format = args("deploy");
        format.format = OutputFormat::Json;
        assert_eq!(run(&fixture.layout, &format).expect("search"), EXIT_OK);

        // An empty result set still renders a well-formed report.
        let mut empty = args("absolutely-not-present");
        empty.format = OutputFormat::Json;
        assert_eq!(run(&fixture.layout, &empty).expect("search"), EXIT_RUNTIME);
    }

    #[test]
    fn searching_every_scope_covers_more_than_the_default_one() {
        let fixture = fixture();
        let agent_dir = fixture.layout.root.join("agents").join("claude");
        fs::create_dir_all(&agent_dir).unwrap();
        fs::write(agent_dir.join("MEMORY.md"), "# Memory\n").unwrap();
        fs::write(
            agent_dir.join("note.md"),
            "---\nname: note\ndescription: agent-scoped\n---\n\nscopedneedle\n",
        )
        .unwrap();

        // The default (global) scope cannot see it.
        assert_eq!(
            run(&fixture.layout, &args("scopedneedle")).expect("search"),
            EXIT_RUNTIME
        );

        let mut all = args("scopedneedle");
        all.all = true;
        assert_eq!(run(&fixture.layout, &all).expect("search"), EXIT_OK);
    }
}

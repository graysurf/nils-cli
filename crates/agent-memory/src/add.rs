//! `agent-memory add` — create a note and its `MEMORY.md` index entry as one
//! guarded operation, so the two never drift.

use std::fs;
use std::io::Read;
use std::path::{Path, PathBuf};

use serde_json::json;

use nils_common::cli_contract::{OutputFormat, schema_version_for};
use nils_common::fs::display_path;

use crate::cli::AddArgs;
use crate::frontmatter::{self, VALID_TYPES};
use crate::{CliError, EXIT_OK, Layout};

pub(crate) fn run(layout: &Layout, args: &AddArgs) -> Result<i32, CliError> {
    if !VALID_TYPES.contains(&args.r#type.as_str()) {
        return Err(CliError::usage(format!(
            "invalid type '{}': expected one of user|feedback|project|reference",
            args.r#type
        )));
    }
    if !frontmatter::is_valid_slug(&args.name) {
        return Err(CliError::usage(format!(
            "invalid name '{}': use ASCII letters, digits, '-' or '_'",
            args.name
        )));
    }

    let scope = args.scope.as_deref().unwrap_or("global");
    let dir = layout.resolve_scope(Some(scope))?;
    if !dir.is_dir() {
        return Err(CliError::runtime(format!(
            "not found: {}",
            display_path(&dir)
        )));
    }

    let note_path = dir.join(format!("{}.md", args.name));
    if note_path.exists() {
        return Err(CliError::runtime(format!(
            "already exists: {}",
            display_path(&note_path)
        )));
    }
    let index_path = dir.join("MEMORY.md");
    if !index_path.is_file() {
        return Err(CliError::runtime(format!(
            "no MEMORY.md in {}",
            display_path(&dir)
        )));
    }

    let body = read_body(args)?;
    let contents = frontmatter::render_note(
        &args.name,
        &args.description,
        &args.r#type,
        args.session_id.as_deref(),
        &body,
    );

    let title = args.title.clone().unwrap_or_else(|| args.name.clone());
    let hook = args
        .hook
        .clone()
        .unwrap_or_else(|| args.description.clone());
    let index_line = format!("- [{title}]({}.md) — {hook}\n", args.name);

    // Write the note first (atomic via temp+rename); then append the index line
    // (also atomic). If the index update fails, roll the note back so the store
    // is never left with a note that has no index entry.
    write_atomic(&note_path, &contents)?;
    if let Err(err) = append_index_line(&index_path, &index_line) {
        let _ = fs::remove_file(&note_path);
        return Err(err);
    }

    let format = if args.json {
        OutputFormat::Json
    } else {
        args.format
    };
    if format.is_json() {
        let doc = json!({
            "schema_version": schema_version_for("agent-memory", "add", 1),
            "ok": true,
            "scope": scope,
            "note": display_path(&note_path),
            "index": display_path(&index_path),
        });
        println!(
            "{}",
            serde_json::to_string(&doc).expect("add report should serialize")
        );
    } else {
        println!("created: {}", display_path(&note_path));
        println!("indexed: {}", display_path(&index_path));
    }
    Ok(EXIT_OK)
}

fn read_body(args: &AddArgs) -> Result<String, CliError> {
    if let Some(path) = &args.body_file {
        return fs::read_to_string(path)
            .map_err(|err| CliError::runtime(format!("failed to read body file {path}: {err}")));
    }
    match args.body.as_deref() {
        Some("-") => {
            let mut buffer = String::new();
            std::io::stdin()
                .read_to_string(&mut buffer)
                .map_err(|err| CliError::runtime(format!("failed to read stdin: {err}")))?;
            Ok(buffer)
        }
        Some(text) => Ok(text.to_string()),
        None => Ok(String::new()),
    }
}

fn write_atomic(path: &Path, contents: &str) -> Result<(), CliError> {
    let tmp = temp_sibling(path);
    fs::write(&tmp, contents).map_err(|err| {
        CliError::runtime(format!("failed to write {}: {err}", display_path(&tmp)))
    })?;
    fs::rename(&tmp, path).map_err(|err| {
        let _ = fs::remove_file(&tmp);
        CliError::runtime(format!("failed to finalize {}: {err}", display_path(path)))
    })
}

fn append_index_line(index_path: &Path, line: &str) -> Result<(), CliError> {
    let mut contents = fs::read_to_string(index_path).map_err(|err| {
        CliError::runtime(format!(
            "failed to read {}: {err}",
            display_path(index_path)
        ))
    })?;
    if !contents.is_empty() && !contents.ends_with('\n') {
        contents.push('\n');
    }
    contents.push_str(line);
    write_atomic(index_path, &contents)
}

fn temp_sibling(path: &Path) -> PathBuf {
    let mut name = path
        .file_name()
        .map(|value| value.to_os_string())
        .unwrap_or_default();
    name.push(".tmp");
    path.with_file_name(name)
}

#[cfg(test)]
mod tests {
    use super::*;

    use pretty_assertions::assert_eq;
    use tempfile::TempDir;

    use crate::{EXIT_RUNTIME, EXIT_USAGE};

    struct Fixture {
        _tmp: TempDir,
        layout: Layout,
        global: PathBuf,
    }

    fn fixture(with_index: bool) -> Fixture {
        let tmp = TempDir::new().unwrap();
        let root = tmp.path().join("agent-memory");
        let global = root.join("global");
        fs::create_dir_all(&global).unwrap();
        if with_index {
            fs::write(global.join("MEMORY.md"), "# Memory\n").unwrap();
        }
        Fixture {
            layout: Layout { root },
            global,
            _tmp: tmp,
        }
    }

    fn args(name: &str, kind: &str) -> AddArgs {
        AddArgs {
            scope: Some("global".to_string()),
            name: name.to_string(),
            r#type: kind.to_string(),
            description: "one-line description".to_string(),
            title: None,
            hook: None,
            body_file: None,
            body: Some("the fact".to_string()),
            session_id: None,
            format: OutputFormat::Text,
            json: false,
        }
    }

    #[test]
    fn a_note_and_its_index_line_are_written_together() {
        let fixture = fixture(true);

        let code = run(&fixture.layout, &args("my-note", "project")).expect("add");

        assert_eq!(code, EXIT_OK);
        let note = fs::read_to_string(fixture.global.join("my-note.md")).expect("note");
        assert!(note.contains("name: my-note"), "{note}");
        assert!(note.contains("the fact"), "{note}");
        let index = fs::read_to_string(fixture.global.join("MEMORY.md")).expect("index");
        assert!(
            index.ends_with("- [my-note](my-note.md) — one-line description\n"),
            "{index}"
        );
    }

    #[test]
    fn an_explicit_title_and_hook_override_the_defaults() {
        let fixture = fixture(true);
        let mut args = args("my-note", "reference");
        args.title = Some("My Note".to_string());
        args.hook = Some("where the dashboard lives".to_string());

        run(&fixture.layout, &args).expect("add");

        let index = fs::read_to_string(fixture.global.join("MEMORY.md")).expect("index");
        assert!(
            index.ends_with("- [My Note](my-note.md) — where the dashboard lives\n"),
            "{index}"
        );
    }

    #[test]
    fn the_type_and_name_vocabularies_are_closed() {
        let fixture = fixture(true);

        let bad_type = run(&fixture.layout, &args("my-note", "musings")).expect_err("bad type");
        assert_eq!(bad_type.exit_code, EXIT_USAGE);
        assert!(bad_type.message.contains("invalid type 'musings'"));

        for name in ["../escape", "with space", ""] {
            let bad_name = run(&fixture.layout, &args(name, "project")).expect_err("bad name");
            assert_eq!(bad_name.exit_code, EXIT_USAGE, "{name}");
            assert!(bad_name.message.contains("invalid name"), "{name}");
        }

        for kind in ["user", "feedback", "project", "reference"] {
            run(&fixture.layout, &args(&format!("note-{kind}"), kind))
                .unwrap_or_else(|err| panic!("{kind} must be accepted: {}", err.message));
        }
    }

    #[test]
    fn a_missing_scope_or_index_is_refused_before_anything_is_written() {
        let missing_scope = fixture(true);
        let mut scoped = args("my-note", "project");
        scoped.scope = Some("agents/absent".to_string());
        let err = run(&missing_scope.layout, &scoped).expect_err("missing scope dir");
        assert_eq!(err.exit_code, EXIT_RUNTIME);
        assert!(err.message.starts_with("not found: "), "{}", err.message);

        // A scope directory without MEMORY.md would leave an unindexed note.
        let no_index = fixture(false);
        let err = run(&no_index.layout, &args("my-note", "project")).expect_err("no index");
        assert_eq!(err.exit_code, EXIT_RUNTIME);
        assert!(
            err.message.starts_with("no MEMORY.md in "),
            "{}",
            err.message
        );
        assert!(!no_index.global.join("my-note.md").exists());
    }

    #[test]
    fn an_existing_note_is_never_overwritten() {
        let fixture = fixture(true);
        fs::write(fixture.global.join("my-note.md"), "original\n").unwrap();

        let err = run(&fixture.layout, &args("my-note", "project")).expect_err("exists");

        assert_eq!(err.exit_code, EXIT_RUNTIME);
        assert!(
            err.message.starts_with("already exists: "),
            "{}",
            err.message
        );
        assert_eq!(
            fs::read_to_string(fixture.global.join("my-note.md")).unwrap(),
            "original\n"
        );
    }

    #[test]
    fn a_failed_index_update_rolls_the_note_back() {
        let fixture = fixture(true);
        // The index itself must stay a real file so the early `is_file()` guard
        // passes and the failure lands on the atomic write instead. Occupying
        // the temp sibling with a directory is what makes that write fail,
        // after the note has already been created.
        fs::create_dir(fixture.global.join("MEMORY.md.tmp")).unwrap();

        let err = run(&fixture.layout, &args("my-note", "project")).expect_err("index write fails");

        assert_eq!(err.exit_code, EXIT_RUNTIME);
        assert!(
            !fixture.global.join("my-note.md").exists(),
            "the note must be rolled back so the store never holds an unindexed note"
        );
        assert_eq!(
            fs::read_to_string(fixture.global.join("MEMORY.md")).unwrap(),
            "# Memory\n",
            "the index must be untouched"
        );
    }

    #[test]
    fn an_index_without_a_trailing_newline_is_repaired_before_appending() {
        let fixture = fixture(true);
        fs::write(fixture.global.join("MEMORY.md"), "# Memory").unwrap();

        run(&fixture.layout, &args("my-note", "project")).expect("add");

        assert_eq!(
            fs::read_to_string(fixture.global.join("MEMORY.md")).unwrap(),
            "# Memory\n- [my-note](my-note.md) — one-line description\n"
        );
    }

    #[test]
    fn the_body_comes_from_the_flag_a_file_or_nothing() {
        let fixture = fixture(true);

        let mut inline = args("inline", "project");
        inline.body = Some("inline body".to_string());
        assert_eq!(read_body(&inline).expect("inline"), "inline body");

        let mut empty = args("empty", "project");
        empty.body = None;
        assert_eq!(read_body(&empty).expect("empty"), "");

        let body_file = fixture.global.join("body.md");
        fs::write(&body_file, "from file\n").unwrap();
        let mut from_file = args("from-file", "project");
        from_file.body = None;
        from_file.body_file = Some(body_file.to_string_lossy().into_owned());
        assert_eq!(read_body(&from_file).expect("file"), "from file\n");

        let mut missing = args("missing", "project");
        missing.body = None;
        missing.body_file = Some("/definitely/not/a/body.md".to_string());
        let err = read_body(&missing).expect_err("missing body file");
        assert_eq!(err.exit_code, EXIT_RUNTIME);
        assert!(err.message.starts_with("failed to read body file"));
    }

    #[test]
    fn json_output_is_selected_by_either_the_flag_or_the_alias() {
        let fixture = fixture(true);
        let mut json_flag = args("via-alias", "project");
        json_flag.json = true;
        run(&fixture.layout, &json_flag).expect("add");

        let mut json_format = args("via-format", "project");
        json_format.format = OutputFormat::Json;
        run(&fixture.layout, &json_format).expect("add");

        let index = fs::read_to_string(fixture.global.join("MEMORY.md")).expect("index");
        assert!(index.contains("via-alias"), "{index}");
        assert!(index.contains("via-format"), "{index}");
    }

    #[test]
    fn the_temp_sibling_never_escapes_the_target_directory() {
        assert_eq!(
            temp_sibling(Path::new("/store/global/note.md")),
            PathBuf::from("/store/global/note.md.tmp")
        );
        // A path with no file name still yields a sibling inside the parent.
        assert_eq!(temp_sibling(Path::new("/")), PathBuf::from("/.tmp"));
    }

    #[test]
    fn an_atomic_write_leaves_no_temp_file_behind() {
        let fixture = fixture(true);
        let target = fixture.global.join("atomic.md");

        write_atomic(&target, "content\n").expect("write");

        assert_eq!(fs::read_to_string(&target).unwrap(), "content\n");
        assert!(!temp_sibling(&target).exists());

        // A target whose parent does not exist fails without panicking.
        let err = write_atomic(&fixture.global.join("absent").join("x.md"), "content")
            .expect_err("no parent directory");
        assert_eq!(err.exit_code, EXIT_RUNTIME);
    }
}

use std::fs;
use std::path::Path;

use nils_test_support::cmd::{CmdOptions, CmdOutput, run_resolved};
use pretty_assertions::assert_eq;

/// Runs `agent-memory` with an isolated memory home, synthetic `HOME`, and no
/// ambient `XDG_CONFIG_HOME`.
fn run(root: &Path, args: &[&str]) -> CmdOutput {
    let options = CmdOptions::new()
        .with_env("AGENT_MEMORY_HOME", &root.to_string_lossy())
        .with_env("HOME", &root.join("home").to_string_lossy())
        .with_env_remove("XDG_CONFIG_HOME");
    run_resolved("agent-memory", args, &options)
}

#[test]
fn no_args_and_help_print_usage() {
    let tmp = tempfile::TempDir::new().expect("tempdir");
    seed_layout(tmp.path());

    let no_args = run(tmp.path(), &[]);
    assert_eq!(no_args.code, 0, "stderr={}", no_args.stderr_text());
    assert!(
        no_args
            .stdout_text()
            .contains("Usage: agent-memory <COMMAND>")
    );

    let help = run(tmp.path(), &["help"]);
    assert_eq!(help.code, 0, "stderr={}", help.stderr_text());
    assert!(help.stdout_text().contains("Usage: agent-memory <COMMAND>"));
}

/// Seeds the minimal store layout used by path, list, and init tests.
fn seed_layout(root: &Path) {
    fs::create_dir_all(root.join("global")).expect("global dir");
    fs::create_dir_all(root.join("agents")).expect("agents dir");
    fs::create_dir_all(root.join("personas")).expect("personas dir");
    fs::write(root.join("global").join("MEMORY.md"), "# Global\n").expect("global memory");
    fs::write(root.join("global").join("user.md"), "# User\n").expect("global note");
}

#[test]
fn resolves_root_and_global_scopes() {
    let tmp = tempfile::TempDir::new().expect("tempdir");
    seed_layout(tmp.path());

    let root_output = run(tmp.path(), &["path"]);
    assert_eq!(root_output.code, 0, "stderr={}", root_output.stderr_text());
    assert_eq!(
        root_output.stdout_text(),
        format!("{}\n", tmp.path().display())
    );

    let global_output = run(tmp.path(), &["path", "global"]);
    assert_eq!(global_output.code, 0);
    assert_eq!(
        global_output.stdout_text(),
        format!("{}/global\n", tmp.path().display())
    );
}

#[test]
fn lists_and_prints_memory_index() {
    let tmp = tempfile::TempDir::new().expect("tempdir");
    seed_layout(tmp.path());

    let list = run(tmp.path(), &["list", "global"]);
    assert_eq!(list.code, 0, "stderr={}", list.stderr_text());
    assert_eq!(list.stdout_text(), "MEMORY.md\nuser.md\n");

    let index = run(tmp.path(), &["index", "global"]);
    assert_eq!(index.code, 0, "stderr={}", index.stderr_text());
    assert_eq!(index.stdout_text(), "# Global\n");
}

#[test]
fn initializes_agent_scope() {
    let tmp = tempfile::TempDir::new().expect("tempdir");
    seed_layout(tmp.path());

    let output = run(tmp.path(), &["init-agent", "codex"]);
    assert_eq!(output.code, 0, "stderr={}", output.stderr_text());
    assert!(output.stdout_text().contains("created:"));
    assert_eq!(
        fs::read_to_string(tmp.path().join("agents/codex/MEMORY.md")).expect("memory"),
        "# Memory index (codex)\n\n"
    );

    let agents = run(tmp.path(), &["agents"]);
    assert_eq!(agents.stdout_text(), "codex\n");
}

#[test]
fn initializes_persona_scope() {
    let tmp = tempfile::TempDir::new().expect("tempdir");
    seed_layout(tmp.path());

    let output = run(tmp.path(), &["init-persona", "work"]);
    assert_eq!(output.code, 0, "stderr={}", output.stderr_text());
    assert!(tmp.path().join("personas/work/CLAUDE.md").is_file());
    assert!(tmp.path().join("personas/work/memory/MEMORY.md").is_file());
    assert!(
        tmp.path()
            .join("personas/work/.claude/settings.local.json")
            .is_file()
    );
}

#[test]
fn resolve_prints_global_and_agent_paths() {
    let tmp = tempfile::TempDir::new().expect("tempdir");
    seed_layout(tmp.path());

    let output = run(tmp.path(), &["resolve", "codex"]);
    assert_eq!(output.code, 0, "stderr={}", output.stderr_text());
    assert_eq!(
        output.stdout_text(),
        format!(
            "global\t{}/global\nagent\t{}/agents/codex\n",
            tmp.path().display(),
            tmp.path().display()
        )
    );
}

#[test]
fn env_prints_exports() {
    let tmp = tempfile::TempDir::new().expect("tempdir");
    seed_layout(tmp.path());

    let output = run(tmp.path(), &["env"]);
    assert_eq!(output.code, 0, "stderr={}", output.stderr_text());
    let stdout = output.stdout_text();
    assert!(stdout.contains("export AGENT_MEMORY_HOME="));
    assert!(stdout.contains("export AGENT_MEMORY_GLOBAL="));
    assert!(stdout.contains("export AGENT_MEMORY_AGENTS="));
    assert!(stdout.contains("export AGENT_MEMORY_PERSONAS="));
}

#[test]
fn doctor_reports_layout() {
    let tmp = tempfile::TempDir::new().expect("tempdir");
    seed_layout(tmp.path());

    let output = run(tmp.path(), &["doctor"]);
    assert_eq!(output.code, 0, "stderr={}", output.stderr_text());
    let stdout = output.stdout_text();
    assert!(stdout.contains("[ok]      root present"));
    assert!(stdout.contains("[ok]      global (real dir)"));
    assert!(stdout.contains("[ok]      agents/"));
    assert!(stdout.contains("[ok]      personas/"));
}

#[test]
fn missing_scope_returns_runtime_error() {
    let tmp = tempfile::TempDir::new().expect("tempdir");
    seed_layout(tmp.path());

    let output = run(tmp.path(), &["list", "missing"]);
    assert_eq!(output.code, 1);
    assert!(output.stderr_text().contains("agent-memory: not found:"));
}

#[test]
fn invalid_id_returns_usage_error() {
    let tmp = tempfile::TempDir::new().expect("tempdir");
    seed_layout(tmp.path());

    let output = run(tmp.path(), &["resolve", "bad/id"]);
    assert_eq!(output.code, 64);
    assert!(output.stderr_text().contains("invalid id"));
}

// ---- `check` command ----------------------------------------------------

/// Build a well-formed note fixture with the frontmatter required by
/// `check` and `list` tests.
fn note(name: &str, ty: &str, body: &str) -> String {
    format!(
        "---\nname: {name}\ndescription: \"summary for {name}\"\nmetadata:\n  node_type: memory\n  type: {ty}\n  originSessionId: 00000000-0000-0000-0000-000000000000\n---\n\n{body}\n"
    )
}

/// Seed a clean `global` scope: two indexed notes, wikilinks all resolve.
fn seed_check_scope(root: &Path) {
    let global = root.join("global");
    fs::create_dir_all(&global).expect("global dir");
    fs::create_dir_all(root.join("agents")).expect("agents dir");
    fs::create_dir_all(root.join("personas")).expect("personas dir");
    fs::write(
        global.join("alpha.md"),
        note("alpha-note", "user", "Alpha body; see [[beta]]."),
    )
    .expect("alpha");
    fs::write(
        global.join("beta.md"),
        note(
            "beta-note",
            "feedback",
            "Beta body.\n\n**Why:** reason.\n\n**How to apply:** steps.",
        ),
    )
    .expect("beta");
    fs::write(
        global.join("MEMORY.md"),
        "# Memory index\n\n- [Alpha](alpha.md) — alpha hook\n- [Beta](beta.md) — beta hook\n",
    )
    .expect("memory");
}

#[test]
fn check_clean_scope_exits_zero() {
    let tmp = tempfile::TempDir::new().expect("tempdir");
    seed_check_scope(tmp.path());

    let out = run(tmp.path(), &["check", "global"]);
    assert_eq!(out.code, 0, "stderr={}", out.stderr_text());
}

#[test]
fn check_defaults_to_global_scope() {
    let tmp = tempfile::TempDir::new().expect("tempdir");
    seed_check_scope(tmp.path());

    let out = run(tmp.path(), &["check"]);
    assert_eq!(out.code, 0, "stderr={}", out.stderr_text());
}

#[test]
fn check_flags_orphan_note() {
    let tmp = tempfile::TempDir::new().expect("tempdir");
    seed_check_scope(tmp.path());
    fs::write(
        tmp.path().join("global/gamma.md"),
        note("gamma-note", "project", "Gamma body."),
    )
    .expect("gamma");

    let out = run(tmp.path(), &["check", "global"]);
    assert_eq!(out.code, 1, "stderr={}", out.stderr_text());
    assert!(
        out.stdout_text().contains("orphan-note"),
        "{}",
        out.stdout_text()
    );
    assert!(
        out.stdout_text().contains("gamma.md"),
        "{}",
        out.stdout_text()
    );
}

#[test]
fn check_flags_broken_index_link() {
    let tmp = tempfile::TempDir::new().expect("tempdir");
    seed_check_scope(tmp.path());
    fs::write(
        tmp.path().join("global/MEMORY.md"),
        "# Memory index\n\n- [Alpha](alpha.md) — alpha hook\n- [Beta](beta.md) — beta hook\n- [Ghost](ghost.md) — missing\n",
    )
    .expect("memory");

    let out = run(tmp.path(), &["check", "global"]);
    assert_eq!(out.code, 1, "stderr={}", out.stderr_text());
    assert!(
        out.stdout_text().contains("index-broken-link"),
        "{}",
        out.stdout_text()
    );
    assert!(
        out.stdout_text().contains("ghost.md"),
        "{}",
        out.stdout_text()
    );
}

#[test]
fn check_flags_missing_required_frontmatter_field() {
    let tmp = tempfile::TempDir::new().expect("tempdir");
    seed_check_scope(tmp.path());
    // Indexed note that omits metadata.type.
    fs::write(
        tmp.path().join("global/gamma.md"),
        "---\nname: gamma-note\ndescription: \"x\"\nmetadata:\n  node_type: memory\n---\n\nBody.\n",
    )
    .expect("gamma");
    fs::write(
        tmp.path().join("global/MEMORY.md"),
        "# Memory index\n\n- [Alpha](alpha.md) — a\n- [Beta](beta.md) — b\n- [Gamma](gamma.md) — g\n",
    )
    .expect("memory");

    let out = run(tmp.path(), &["check", "global"]);
    assert_eq!(out.code, 1, "stderr={}", out.stderr_text());
    assert!(out.stdout_text().contains("type"), "{}", out.stdout_text());
}

#[test]
fn check_flags_invalid_type_enum() {
    let tmp = tempfile::TempDir::new().expect("tempdir");
    seed_check_scope(tmp.path());
    fs::write(
        tmp.path().join("global/gamma.md"),
        note("gamma-note", "bogus", "Body."),
    )
    .expect("gamma");
    fs::write(
        tmp.path().join("global/MEMORY.md"),
        "# Memory index\n\n- [Alpha](alpha.md) — a\n- [Beta](beta.md) — b\n- [Gamma](gamma.md) — g\n",
    )
    .expect("memory");

    let out = run(tmp.path(), &["check", "global"]);
    assert_eq!(out.code, 1, "stderr={}", out.stderr_text());
    assert!(
        out.stdout_text().contains("type-invalid") || out.stdout_text().contains("bogus"),
        "{}",
        out.stdout_text()
    );
}

#[test]
fn check_flags_missing_frontmatter() {
    let tmp = tempfile::TempDir::new().expect("tempdir");
    seed_check_scope(tmp.path());
    fs::write(
        tmp.path().join("global/gamma.md"),
        "# Gamma\n\nNo frontmatter.\n",
    )
    .expect("gamma");
    fs::write(
        tmp.path().join("global/MEMORY.md"),
        "# Memory index\n\n- [Alpha](alpha.md) — a\n- [Beta](beta.md) — b\n- [Gamma](gamma.md) — g\n",
    )
    .expect("memory");

    let out = run(tmp.path(), &["check", "global"]);
    assert_eq!(out.code, 1, "stderr={}", out.stderr_text());
    assert!(
        out.stdout_text().contains("frontmatter-missing"),
        "{}",
        out.stdout_text()
    );
}

#[test]
fn check_accepts_handauthored_note_and_strict_promotes_warning() {
    let tmp = tempfile::TempDir::new().expect("tempdir");
    seed_check_scope(tmp.path());
    // Hand-authored: name/description/type present, no node_type/originSessionId.
    fs::write(
        tmp.path().join("global/gamma.md"),
        "---\nname: gamma-note\ndescription: \"x\"\nmetadata:\n  type: reference\n---\n\nBody.\n",
    )
    .expect("gamma");
    fs::write(
        tmp.path().join("global/MEMORY.md"),
        "# Memory index\n\n- [Alpha](alpha.md) — a\n- [Beta](beta.md) — b\n- [Gamma](gamma.md) — g\n",
    )
    .expect("memory");

    let lenient = run(tmp.path(), &["check", "global"]);
    assert_eq!(lenient.code, 0, "stderr={}", lenient.stderr_text());

    let strict = run(tmp.path(), &["check", "global", "--strict"]);
    assert_eq!(strict.code, 1, "stderr={}", strict.stderr_text());
}

#[test]
fn check_dangling_wikilink_is_warn() {
    let tmp = tempfile::TempDir::new().expect("tempdir");
    seed_check_scope(tmp.path());
    // alpha already references [[beta]] (resolves); add a dangling forward-ref.
    fs::write(
        tmp.path().join("global/alpha.md"),
        note(
            "alpha-note",
            "user",
            "Alpha; see [[beta]] and [[not_yet_written]].",
        ),
    )
    .expect("alpha");

    let lenient = run(tmp.path(), &["check", "global"]);
    assert_eq!(lenient.code, 0, "stderr={}", lenient.stderr_text());
    assert!(
        lenient.stdout_text().contains("dangling-wikilink"),
        "{}",
        lenient.stdout_text()
    );
    assert!(
        lenient.stdout_text().contains("not_yet_written"),
        "{}",
        lenient.stdout_text()
    );

    let strict = run(tmp.path(), &["check", "global", "--strict"]);
    assert_eq!(strict.code, 1, "stderr={}", strict.stderr_text());
}

#[test]
fn check_json_emits_findings_and_counts() {
    let tmp = tempfile::TempDir::new().expect("tempdir");
    seed_check_scope(tmp.path());
    fs::write(
        tmp.path().join("global/gamma.md"),
        note("gamma-note", "project", "Gamma body."),
    )
    .expect("gamma");

    // Canonical `--format json` surface.
    let out = run(tmp.path(), &["check", "global", "--format", "json"]);
    assert_eq!(out.code, 1, "stderr={}", out.stderr_text());
    let stdout = out.stdout_text();
    assert!(
        stdout.contains("\"schema_version\":\"cli.agent-memory.check.v1\""),
        "{stdout}"
    );
    assert!(stdout.contains("\"ok\":false"), "{stdout}");
    assert!(stdout.contains("\"kind\":\"orphan-note\""), "{stdout}");
    assert!(stdout.contains("\"file\":\"gamma.md\""), "{stdout}");
    assert!(stdout.contains("\"severity\":\"error\""), "{stdout}");

    // Hidden `--json` alias produces identical output.
    let aliased = run(tmp.path(), &["check", "global", "--json"]);
    assert_eq!(aliased.code, 1, "stderr={}", aliased.stderr_text());
    assert_eq!(aliased.stdout_text(), stdout);
}

#[test]
fn check_all_sweeps_agent_scopes() {
    let tmp = tempfile::TempDir::new().expect("tempdir");
    seed_check_scope(tmp.path());
    // A second scope with an orphan note.
    let codex = tmp.path().join("agents/codex");
    fs::create_dir_all(&codex).expect("codex dir");
    fs::write(codex.join("MEMORY.md"), "# Memory index (codex)\n\n").expect("codex memory");
    fs::write(
        codex.join("orphan.md"),
        note("orphan-note", "user", "Orphan."),
    )
    .expect("orphan");

    let out = run(tmp.path(), &["check", "--all"]);
    assert_eq!(out.code, 1, "stderr={}", out.stderr_text());
    assert!(
        out.stdout_text().contains("agents/codex"),
        "{}",
        out.stdout_text()
    );
    assert!(
        out.stdout_text().contains("orphan-note"),
        "{}",
        out.stdout_text()
    );
}

// ---- `add` command ------------------------------------------------------

/// Seed a writable `global` scope with an empty index for add/write tests.
fn seed_writable_global(root: &Path) {
    fs::create_dir_all(root.join("global")).expect("global dir");
    fs::create_dir_all(root.join("agents")).expect("agents dir");
    fs::create_dir_all(root.join("personas")).expect("personas dir");
    fs::write(root.join("global/MEMORY.md"), "# Memory index\n\n").expect("memory");
}

#[test]
fn add_creates_note_and_index_then_check_is_clean() {
    let tmp = tempfile::TempDir::new().expect("tempdir");
    seed_writable_global(tmp.path());

    let out = run(
        tmp.path(),
        &[
            "add",
            "global",
            "--name",
            "new-note",
            "--type",
            "feedback",
            "--description",
            "desc text",
            "--hook",
            "hook text",
            "--body",
            "Body with content.",
            "--session-id",
            "00000000-0000-0000-0000-000000000000",
        ],
    );
    assert_eq!(out.code, 0, "stderr={}", out.stderr_text());

    let note = fs::read_to_string(tmp.path().join("global/new-note.md")).expect("note file");
    assert!(note.contains("name: new-note"), "{note}");
    assert!(note.contains("type: feedback"), "{note}");
    assert!(note.contains("node_type: memory"), "{note}");
    assert!(note.contains("Body with content."), "{note}");

    let index = fs::read_to_string(tmp.path().join("global/MEMORY.md")).expect("index");
    assert!(
        index.contains("- [new-note](new-note.md) — hook text"),
        "{index}"
    );

    // Parity must be intact after add.
    let check = run(tmp.path(), &["check", "global"]);
    assert_eq!(check.code, 0, "stderr={}", check.stderr_text());
}

#[test]
fn add_refuses_duplicate_slug() {
    let tmp = tempfile::TempDir::new().expect("tempdir");
    seed_writable_global(tmp.path());

    let first = run(
        tmp.path(),
        &[
            "add",
            "global",
            "--name",
            "dup",
            "--type",
            "user",
            "--description",
            "x",
        ],
    );
    assert_eq!(first.code, 0, "stderr={}", first.stderr_text());

    let second = run(
        tmp.path(),
        &[
            "add",
            "global",
            "--name",
            "dup",
            "--type",
            "user",
            "--description",
            "y",
        ],
    );
    assert_eq!(second.code, 1, "stderr={}", second.stderr_text());
    assert!(second.stderr_text().contains("already exists"));
}

#[test]
fn add_rejects_invalid_type() {
    let tmp = tempfile::TempDir::new().expect("tempdir");
    seed_writable_global(tmp.path());

    let out = run(
        tmp.path(),
        &[
            "add",
            "global",
            "--name",
            "bad",
            "--type",
            "bogus",
            "--description",
            "x",
        ],
    );
    assert_eq!(out.code, 64, "stderr={}", out.stderr_text());
    assert!(out.stderr_text().contains("invalid type"));
}

#[test]
fn add_omits_session_id_when_not_supplied() {
    let tmp = tempfile::TempDir::new().expect("tempdir");
    seed_writable_global(tmp.path());

    let out = run(
        tmp.path(),
        &[
            "add",
            "global",
            "--name",
            "hand",
            "--type",
            "reference",
            "--description",
            "x",
        ],
    );
    assert_eq!(out.code, 0, "stderr={}", out.stderr_text());
    let note = fs::read_to_string(tmp.path().join("global/hand.md")).expect("note");
    assert!(!note.contains("originSessionId"), "{note}");
}

// ---- `list --json` / `--type` -------------------------------------------

#[test]
fn list_json_emits_frontmatter_fields() {
    let tmp = tempfile::TempDir::new().expect("tempdir");
    seed_check_scope(tmp.path());

    let out = run(tmp.path(), &["list", "global", "--json"]);
    assert_eq!(out.code, 0, "stderr={}", out.stderr_text());
    let stdout = out.stdout_text();
    assert!(
        stdout.contains("\"schema_version\":\"cli.agent-memory.list.v1\""),
        "{stdout}"
    );
    assert!(stdout.contains("\"name\":\"alpha-note\""), "{stdout}");
    assert!(stdout.contains("\"type\":\"user\""), "{stdout}");
    assert!(stdout.contains("\"mtime\":"), "{stdout}");
    // MEMORY.md is not a note.
    assert!(!stdout.contains("MEMORY.md"), "{stdout}");
}

#[test]
fn list_type_filters_in_text_mode() {
    let tmp = tempfile::TempDir::new().expect("tempdir");
    seed_check_scope(tmp.path());

    let out = run(tmp.path(), &["list", "global", "--type", "feedback"]);
    assert_eq!(out.code, 0, "stderr={}", out.stderr_text());
    let stdout = out.stdout_text();
    assert!(stdout.contains("beta.md"), "{stdout}");
    assert!(!stdout.contains("alpha.md"), "{stdout}");
    assert!(!stdout.contains("MEMORY.md"), "{stdout}");
}

#[test]
fn list_default_output_is_unchanged() {
    let tmp = tempfile::TempDir::new().expect("tempdir");
    seed_check_scope(tmp.path());

    let out = run(tmp.path(), &["list", "global"]);
    assert_eq!(out.code, 0, "stderr={}", out.stderr_text());
    assert_eq!(out.stdout_text(), "MEMORY.md\nalpha.md\nbeta.md\n");
}

// ---- `search` command ---------------------------------------------------

/// Build a note fixture whose description and body can be searched
/// independently.
fn note_with(name: &str, ty: &str, description: &str, body: &str) -> String {
    format!(
        "---\nname: {name}\ndescription: \"{description}\"\nmetadata:\n  node_type: memory\n  type: {ty}\n  originSessionId: 00000000-0000-0000-0000-000000000000\n---\n\n{body}\n"
    )
}

#[test]
fn search_matches_both_body_and_description() {
    let tmp = tempfile::TempDir::new().expect("tempdir");
    let global = tmp.path().join("global");
    fs::create_dir_all(&global).expect("global");
    fs::create_dir_all(tmp.path().join("agents")).expect("agents");
    fs::create_dir_all(tmp.path().join("personas")).expect("personas");
    fs::write(
        global.join("bodyonly.md"),
        note_with(
            "bodyonly",
            "user",
            "plain description",
            "Contains unique_body_token here.",
        ),
    )
    .expect("bodyonly");
    fs::write(
        global.join("desconly.md"),
        note_with(
            "desconly",
            "user",
            "has unique_desc_token inside",
            "Plain body.",
        ),
    )
    .expect("desconly");
    fs::write(
        global.join("MEMORY.md"),
        "# Memory index\n\n- [bodyonly](bodyonly.md) — b\n- [desconly](desconly.md) — d\n",
    )
    .expect("memory");

    let body_hit = run(tmp.path(), &["search", "unique_body_token", "global"]);
    assert_eq!(body_hit.code, 0, "stderr={}", body_hit.stderr_text());
    assert!(
        body_hit.stdout_text().contains("bodyonly.md"),
        "{}",
        body_hit.stdout_text()
    );

    let desc_hit = run(tmp.path(), &["search", "unique_desc_token", "global"]);
    assert_eq!(desc_hit.code, 0, "stderr={}", desc_hit.stderr_text());
    assert!(
        desc_hit.stdout_text().contains("desconly.md"),
        "{}",
        desc_hit.stdout_text()
    );

    let no_hit = run(tmp.path(), &["search", "zzzz_no_match", "global"]);
    assert_eq!(no_hit.code, 1, "stderr={}", no_hit.stderr_text());
}

#[test]
fn search_all_covers_agent_scopes() {
    let tmp = tempfile::TempDir::new().expect("tempdir");
    seed_writable_global(tmp.path());
    let codex = tmp.path().join("agents/codex");
    fs::create_dir_all(&codex).expect("codex");
    fs::write(codex.join("MEMORY.md"), "# Memory index (codex)\n\n").expect("codex memory");
    fs::write(
        codex.join("found.md"),
        note_with("found", "user", "d", "Body with crossscopeterm inside."),
    )
    .expect("found");

    let out = run(tmp.path(), &["search", "crossscopeterm", "--all"]);
    assert_eq!(out.code, 0, "stderr={}", out.stderr_text());
    assert!(
        out.stdout_text().contains("agents/codex"),
        "{}",
        out.stdout_text()
    );
    assert!(
        out.stdout_text().contains("found.md"),
        "{}",
        out.stdout_text()
    );
}

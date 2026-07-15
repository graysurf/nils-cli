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
fn check_flags_duplicate_frontmatter() {
    let tmp = tempfile::TempDir::new().expect("tempdir");
    seed_check_scope(tmp.path());
    fs::write(
        tmp.path().join("global/alpha.md"),
        "---\nname: alpha-note\ndescription: \"summary for alpha-note\"\nmetadata:\n  node_type: memory\n  type: user\n  originSessionId: 00000000-0000-0000-0000-000000000000\n---\n\n---\nname: duplicate\ndescription: \"raw duplicate\"\nmetadata:\n  type: user\n---\n\nAlpha body; see [[beta]].\n",
    )
    .expect("alpha");

    let out = run(tmp.path(), &["check", "global", "--strict"]);
    assert_eq!(out.code, 1, "stderr={}", out.stderr_text());
    assert!(
        out.stdout_text().contains("frontmatter-duplicate"),
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

// ---- recall profiles and candidate lifecycle ----------------------------

fn seed_recall_layout(root: &Path) {
    seed_writable_global(root);
    fs::create_dir_all(root.join("profiles/startup")).expect("startup profile");
    fs::create_dir_all(root.join("candidates/claude")).expect("claude candidates");
    fs::create_dir_all(root.join("candidates/codex")).expect("codex candidates");
    fs::create_dir_all(root.join("candidates/hermes")).expect("hermes candidates");
    fs::write(
        root.join("profiles/startup/MEMORY.md"),
        "# Startup\n\n- [Routing](../../global/routing.md) — route\n",
    )
    .expect("startup index");
    fs::write(
        root.join("global/routing.md"),
        note_with("routing", "reference", "routing description", "route body"),
    )
    .expect("routing note");
    fs::write(
        root.join("global/MEMORY.md"),
        "# Memory index\n\n- [Routing](routing.md) — route\n",
    )
    .expect("global index");
    for producer in ["claude", "codex", "hermes"] {
        fs::write(
            root.join(format!("candidates/{producer}/MEMORY.md")),
            format!("# {producer} candidates\n\n"),
        )
        .expect("candidate index");
    }
}

#[test]
fn recall_startup_is_bounded_and_has_json_contract() {
    let tmp = tempfile::TempDir::new().expect("tempdir");
    seed_recall_layout(tmp.path());

    let text = run(tmp.path(), &["recall", "startup"]);
    assert_eq!(text.code, 0, "stderr={}", text.stderr_text());
    assert!(text.stdout_text().contains("# Startup"));
    assert!(!text.stdout_text().contains("# Memory index"));

    let json = run(tmp.path(), &["recall", "startup", "--format", "json"]);
    assert_eq!(json.code, 0, "stderr={}", json.stderr_text());
    assert!(
        json.stdout_text()
            .contains("\"schema_version\":\"cli.agent-memory.recall-startup.v1\"")
    );
    assert!(json.stdout_text().contains("\"trust\":\"untrusted\""));

    let too_small = run(tmp.path(), &["recall", "startup", "--max-bytes", "8"]);
    assert_eq!(too_small.code, 1);
    assert!(too_small.stderr_text().contains("exceeds 8 bytes"));
}

#[cfg(unix)]
#[test]
fn recall_startup_rejects_symlink_profile_directory() {
    let tmp = tempfile::TempDir::new().expect("tempdir");
    seed_recall_layout(tmp.path());
    fs::remove_dir_all(tmp.path().join("profiles/startup")).expect("remove startup");
    std::os::unix::fs::symlink(
        tmp.path().join("global"),
        tmp.path().join("profiles/startup"),
    )
    .expect("profile symlink");

    let out = run(tmp.path(), &["recall", "startup"]);
    assert_eq!(out.code, 1);
    assert!(out.stderr_text().contains("non-symlink directory"));
}

#[test]
fn recall_on_demand_searches_curated_global_only() {
    let tmp = tempfile::TempDir::new().expect("tempdir");
    seed_recall_layout(tmp.path());
    fs::write(
        tmp.path().join("candidates/codex/secret.md"),
        "candidate_only_token",
    )
    .expect("candidate");

    let hit = run(tmp.path(), &["recall", "on-demand", "route body"]);
    assert_eq!(hit.code, 0, "stderr={}", hit.stderr_text());
    assert!(hit.stdout_text().contains("global/routing.md"));

    let no_candidate = run(tmp.path(), &["recall", "on-demand", "candidate_only_token"]);
    assert_eq!(no_candidate.code, 1);
    assert!(no_candidate.stdout_text().is_empty());
}

#[test]
fn candidate_add_and_recall_are_producer_isolated() {
    let tmp = tempfile::TempDir::new().expect("tempdir");
    seed_writable_global(tmp.path());

    let added = run(
        tmp.path(),
        &[
            "candidate",
            "add",
            "codex",
            "--name",
            "stable-preference",
            "--body",
            "Prefer bounded startup recall.",
            "--format",
            "json",
        ],
    );
    assert_eq!(added.code, 0, "stderr={}", added.stderr_text());
    assert!(added.stdout_text().contains("candidate-add.v1"));
    assert!(
        tmp.path()
            .join("candidates/codex/stable-preference.md")
            .is_file()
    );

    let recalled = run(
        tmp.path(),
        &["recall", "candidates", "codex", "--format", "json"],
    );
    assert_eq!(recalled.code, 0, "stderr={}", recalled.stderr_text());
    let stdout = recalled.stdout_text();
    assert!(stdout.contains("recall-candidates.v1"), "{stdout}");
    assert!(stdout.contains("\"trust\":\"untrusted\""), "{stdout}");
    assert!(stdout.contains("stable-preference.md"), "{stdout}");
    assert!(!stdout.contains("candidates/claude"), "{stdout}");
}

#[test]
fn candidate_promotion_is_dry_run_first_then_atomic_apply() {
    let tmp = tempfile::TempDir::new().expect("tempdir");
    seed_recall_layout(tmp.path());
    let source = tmp.path().join("candidates/codex/preference.md");
    fs::write(&source, "Prefer explicit promotion.\n").expect("candidate");
    fs::write(
        tmp.path().join("candidates/codex/MEMORY.md"),
        "# Codex candidates\n\n- [Preference](preference.md) — review\n",
    )
    .expect("candidate index");

    let args = [
        "candidate",
        "promote",
        "codex",
        "preference",
        "--type",
        "feedback",
        "--description",
        "Use explicit memory promotion",
        "--title",
        "Explicit promotion",
        "--hook",
        "review candidates before global",
        "--session-id",
        "00000000-0000-0000-0000-000000000000",
        "--format",
        "json",
    ];
    let dry_run = run(tmp.path(), &args);
    assert_eq!(dry_run.code, 0, "stderr={}", dry_run.stderr_text());
    assert!(dry_run.stdout_text().contains("\"applied\":false"));
    assert!(
        dry_run
            .stdout_text()
            .contains("\"trust\":\"untrusted-candidate\"")
    );
    assert!(source.is_file(), "dry-run must retain source");
    assert!(!tmp.path().join("global/preference.md").exists());

    let mut apply_args = args.to_vec();
    apply_args.push("--apply");
    let applied = run(tmp.path(), &apply_args);
    assert_eq!(applied.code, 0, "stderr={}", applied.stderr_text());
    assert!(applied.stdout_text().contains("\"applied\":true"));
    assert!(
        applied
            .stdout_text()
            .contains("\"trust\":\"curated-after-explicit-apply\"")
    );
    assert!(!source.exists(), "applied promotion removes source");

    let promoted =
        fs::read_to_string(tmp.path().join("global/preference.md")).expect("promoted note");
    assert!(promoted.contains("name: preference"), "{promoted}");
    assert!(promoted.contains("type: feedback"), "{promoted}");
    assert!(
        promoted.contains("Prefer explicit promotion."),
        "{promoted}"
    );
    let global_index =
        fs::read_to_string(tmp.path().join("global/MEMORY.md")).expect("global index");
    assert!(global_index.contains("[Explicit promotion](preference.md)"));
    let candidate_index =
        fs::read_to_string(tmp.path().join("candidates/codex/MEMORY.md")).expect("candidate index");
    assert!(!candidate_index.contains("preference.md"));

    let check = run(tmp.path(), &["check", "global", "--strict"]);
    assert_eq!(check.code, 0, "stderr={}", check.stderr_text());
}

#[test]
fn candidate_promotion_with_frontmatter_emits_single_header() {
    let tmp = tempfile::TempDir::new().expect("tempdir");
    seed_recall_layout(tmp.path());
    let source = tmp.path().join("candidates/codex/frontmatter-candidate.md");
    fs::write(
        &source,
        "---\nname: frontmatter-candidate\ndescription: \"Untrusted candidate description\"\nmetadata:\n  type: project\n---\n\nPromote this candidate body without its original header.\n",
    )
    .expect("candidate");
    fs::write(
        tmp.path().join("candidates/codex/MEMORY.md"),
        "# Codex candidates\n\n- [Frontmatter candidate](frontmatter-candidate.md) — review\n",
    )
    .expect("candidate index");

    let applied = run(
        tmp.path(),
        &[
            "candidate",
            "promote",
            "codex",
            "frontmatter-candidate",
            "--type",
            "feedback",
            "--description",
            "Canonical promoted description",
            "--session-id",
            "00000000-0000-0000-0000-000000000000",
            "--apply",
        ],
    );
    assert_eq!(applied.code, 0, "stderr={}", applied.stderr_text());

    let promoted = fs::read_to_string(tmp.path().join("global/frontmatter-candidate.md"))
        .expect("promoted note");
    assert_eq!(
        promoted.lines().filter(|line| line.trim() == "---").count(),
        2,
        "{promoted}"
    );
    assert!(
        promoted.contains("description: \"Canonical promoted description\""),
        "{promoted}"
    );
    assert!(promoted.contains("type: feedback"), "{promoted}");
    assert!(
        promoted.contains("Promote this candidate body without its original header."),
        "{promoted}"
    );
    assert!(
        !promoted.contains("Untrusted candidate description"),
        "{promoted}"
    );

    let check = run(tmp.path(), &["check", "global", "--strict"]);
    assert_eq!(
        check.code,
        0,
        "stdout={} stderr={}",
        check.stdout_text(),
        check.stderr_text()
    );
}

#[test]
fn candidate_promotion_preserves_opaque_thematic_rule_body() {
    let tmp = tempfile::TempDir::new().expect("tempdir");
    seed_recall_layout(tmp.path());
    fs::write(
        tmp.path().join("candidates/codex/thematic-rules.md"),
        "---\nImportant first section.\n---\n\nRemaining candidate body.\n",
    )
    .expect("candidate");

    let applied = run(
        tmp.path(),
        &[
            "candidate",
            "promote",
            "codex",
            "thematic-rules",
            "--type",
            "reference",
            "--description",
            "Preserve opaque Markdown",
            "--session-id",
            "00000000-0000-0000-0000-000000000000",
            "--apply",
        ],
    );
    assert_eq!(applied.code, 0, "stderr={}", applied.stderr_text());

    let promoted =
        fs::read_to_string(tmp.path().join("global/thematic-rules.md")).expect("promoted note");
    assert!(
        promoted.contains("---\nImportant first section.\n---\n\nRemaining candidate body."),
        "{promoted}"
    );

    let text_check = run(tmp.path(), &["check", "global", "--strict"]);
    assert_eq!(
        text_check.code,
        0,
        "stdout={} stderr={}",
        text_check.stdout_text(),
        text_check.stderr_text()
    );
    let json_check = run(tmp.path(), &["check", "global", "--format", "json"]);
    assert_eq!(json_check.code, 0, "stderr={}", json_check.stderr_text());
    let report: serde_json::Value =
        serde_json::from_str(json_check.stdout_text().trim()).expect("check json");
    assert_eq!(report["ok"], true, "{report}");
}

#[test]
fn candidate_promotion_preserves_indented_fence_example() {
    let tmp = tempfile::TempDir::new().expect("tempdir");
    seed_recall_layout(tmp.path());
    fs::write(
        tmp.path().join("candidates/codex/indented-example.md"),
        "    ---\n    name: code-example\n    description: example data\n    ---\n\nRetained code sample.\n",
    )
    .expect("candidate");

    let applied = run(
        tmp.path(),
        &[
            "candidate",
            "promote",
            "codex",
            "indented-example",
            "--type",
            "reference",
            "--description",
            "Preserve indented Markdown",
            "--session-id",
            "00000000-0000-0000-0000-000000000000",
            "--apply",
        ],
    );
    assert_eq!(applied.code, 0, "stderr={}", applied.stderr_text());

    let promoted =
        fs::read_to_string(tmp.path().join("global/indented-example.md")).expect("promoted note");
    assert!(
        promoted.contains(
            "    ---\n    name: code-example\n    description: example data\n    ---\n\nRetained code sample."
        ),
        "{promoted}"
    );

    let check = run(tmp.path(), &["check", "global", "--strict"]);
    assert_eq!(
        check.code,
        0,
        "stdout={} stderr={}",
        check.stdout_text(),
        check.stderr_text()
    );
}

#[cfg(unix)]
#[test]
fn candidate_promotion_supports_valid_global_symlink_layout() {
    let tmp = tempfile::TempDir::new().expect("tempdir");
    seed_recall_layout(tmp.path());
    let global_real = tmp.path().join("global-real");
    fs::rename(tmp.path().join("global"), &global_real).expect("move global");
    std::os::unix::fs::symlink(&global_real, tmp.path().join("global")).expect("global symlink");
    fs::write(
        tmp.path().join("candidates/codex/symlink-compatible.md"),
        "Compatible body.\n",
    )
    .expect("candidate");

    let out = run(
        tmp.path(),
        &[
            "candidate",
            "promote",
            "codex",
            "symlink-compatible",
            "--type",
            "reference",
            "--description",
            "Preserve supported global symlinks",
            "--session-id",
            "00000000-0000-0000-0000-000000000000",
            "--apply",
        ],
    );
    assert_eq!(out.code, 0, "stderr={}", out.stderr_text());
    assert!(global_real.join("symlink-compatible.md").is_file());
    assert!(
        fs::read_to_string(global_real.join("MEMORY.md"))
            .expect("global index")
            .contains("symlink-compatible.md")
    );
}

#[test]
fn candidate_promotion_removes_native_index_filename_reference() {
    let tmp = tempfile::TempDir::new().expect("tempdir");
    seed_recall_layout(tmp.path());
    fs::write(
        tmp.path().join("candidates/codex/native.md"),
        "Native candidate body.\n",
    )
    .expect("candidate");
    fs::write(
        tmp.path().join("candidates/codex/MEMORY.md"),
        "# Native index\n\nRemember native.md as a proposed topic.\n",
    )
    .expect("native index");

    let out = run(
        tmp.path(),
        &[
            "candidate",
            "promote",
            "codex",
            "native",
            "--type",
            "reference",
            "--description",
            "Native candidate",
            "--session-id",
            "00000000-0000-0000-0000-000000000000",
            "--apply",
        ],
    );
    assert_eq!(out.code, 0, "stderr={}", out.stderr_text());
    let index =
        fs::read_to_string(tmp.path().join("candidates/codex/MEMORY.md")).expect("candidate index");
    assert!(!index.contains("native.md"), "{index}");
}

#[test]
fn candidate_promotion_preserves_unrelated_suffix_filename_reference() {
    let tmp = tempfile::TempDir::new().expect("tempdir");
    seed_recall_layout(tmp.path());
    let producer = tmp.path().join("candidates/codex");
    fs::write(producer.join("foo.md"), "Target candidate.\n").expect("target candidate");
    fs::write(producer.join("myfoo.md"), "Unrelated candidate.\n").expect("unrelated candidate");
    fs::write(
        producer.join("MEMORY.md"),
        "# Native index\n\nRemember foo.md as the target.\nRemember myfoo.md as unrelated.\n",
    )
    .expect("native index");

    let out = run(
        tmp.path(),
        &[
            "candidate",
            "promote",
            "codex",
            "foo",
            "--type",
            "reference",
            "--description",
            "Target candidate",
            "--session-id",
            "00000000-0000-0000-0000-000000000000",
            "--apply",
        ],
    );
    assert_eq!(out.code, 0, "stderr={}", out.stderr_text());
    let index = fs::read_to_string(producer.join("MEMORY.md")).expect("candidate index");
    assert!(!index.contains("Remember foo.md as the target."), "{index}");
    assert!(index.contains("Remember myfoo.md as unrelated."), "{index}");
    assert!(producer.join("myfoo.md").is_file());
}

#[test]
fn candidate_promotion_rejects_multiline_metadata_without_writes() {
    for (label, extra) in [
        (
            "description",
            vec!["--description", "bad\nmetadata: injected"],
        ),
        (
            "title",
            vec!["--description", "valid", "--title", "bad\n- injected"],
        ),
        (
            "hook",
            vec!["--description", "valid", "--hook", "bad\n- injected"],
        ),
        (
            "session",
            vec![
                "--description",
                "valid",
                "--session-id",
                "bad\nmetadata: injected",
            ],
        ),
    ] {
        let tmp = tempfile::TempDir::new().expect("tempdir");
        seed_recall_layout(tmp.path());
        let source = tmp.path().join("candidates/codex/reject.md");
        fs::write(&source, "Candidate body.\n").expect("candidate");
        let mut args = vec![
            "candidate",
            "promote",
            "codex",
            "reject",
            "--type",
            "reference",
        ];
        args.extend(extra);
        if label != "session" {
            args.extend(["--session-id", "00000000-0000-0000-0000-000000000000"]);
        }
        args.push("--apply");

        let out = run(tmp.path(), &args);
        assert_eq!(out.code, 64, "{label}: stderr={}", out.stderr_text());
        assert!(
            out.stderr_text().contains("single-line"),
            "{label}: {}",
            out.stderr_text()
        );
        assert!(source.is_file(), "{label}: source must remain");
        assert!(
            !tmp.path().join("global/reject.md").exists(),
            "{label}: destination must not exist"
        );
    }
}

#[test]
fn candidate_promotion_requires_session_provenance() {
    let tmp = tempfile::TempDir::new().expect("tempdir");
    seed_recall_layout(tmp.path());
    fs::write(
        tmp.path().join("candidates/codex/no-session.md"),
        "Candidate body.\n",
    )
    .expect("candidate");

    let out = run(
        tmp.path(),
        &[
            "candidate",
            "promote",
            "codex",
            "no-session",
            "--type",
            "reference",
            "--description",
            "Missing provenance",
            "--apply",
        ],
    );
    assert_eq!(out.code, 64);
    assert!(out.stderr_text().contains("--session-id"));
    assert!(!tmp.path().join("global/no-session.md").exists());
}

#[test]
fn new_json_commands_emit_structured_runtime_errors() {
    let tmp = tempfile::TempDir::new().expect("tempdir");
    seed_recall_layout(tmp.path());

    let startup = run(
        tmp.path(),
        &["recall", "startup", "--max-bytes", "8", "--format", "json"],
    );
    assert_eq!(startup.code, 1);
    assert!(startup.stderr_text().is_empty());
    let startup_json: serde_json::Value =
        serde_json::from_str(startup.stdout_text().trim()).expect("startup error json");
    assert_eq!(startup_json["ok"], false);
    assert_eq!(startup_json["error"]["code"], "runtime-error");

    let promote = run(
        tmp.path(),
        &[
            "candidate",
            "promote",
            "codex",
            "missing",
            "--type",
            "reference",
            "--description",
            "Missing source",
            "--session-id",
            "00000000-0000-0000-0000-000000000000",
            "--format",
            "json",
        ],
    );
    assert_eq!(promote.code, 1);
    assert!(promote.stderr_text().is_empty());
    let promote_json: serde_json::Value =
        serde_json::from_str(promote.stdout_text().trim()).expect("promote error json");
    assert_eq!(promote_json["ok"], false);
    assert_eq!(promote_json["error"]["code"], "runtime-error");
}

#[test]
fn candidate_commands_reject_traversal_and_symlink_sources() {
    let tmp = tempfile::TempDir::new().expect("tempdir");
    seed_recall_layout(tmp.path());

    let traversal = run(
        tmp.path(),
        &[
            "candidate",
            "add",
            "../escape",
            "--name",
            "bad",
            "--body",
            "no",
        ],
    );
    assert_eq!(traversal.code, 64);
    assert!(!tmp.path().join("escape/bad.md").exists());

    #[cfg(unix)]
    {
        std::os::unix::fs::symlink(
            tmp.path().join("global/routing.md"),
            tmp.path().join("candidates/codex/link.md"),
        )
        .expect("symlink");
        let linked = run(
            tmp.path(),
            &[
                "candidate",
                "promote",
                "codex",
                "link",
                "--type",
                "reference",
                "--description",
                "must reject symlink",
                "--session-id",
                "00000000-0000-0000-0000-000000000000",
                "--apply",
            ],
        );
        assert_eq!(linked.code, 1);
        assert!(linked.stderr_text().contains("symlink"));
        assert!(!tmp.path().join("global/link.md").exists());
    }
}

#[test]
fn explicit_profile_and_candidate_scopes_resolve_without_escape() {
    let tmp = tempfile::TempDir::new().expect("tempdir");
    seed_recall_layout(tmp.path());

    let profile = run(tmp.path(), &["path", "profiles/startup"]);
    assert_eq!(profile.code, 0, "stderr={}", profile.stderr_text());
    assert_eq!(
        profile.stdout_text(),
        format!("{}/profiles/startup\n", tmp.path().display())
    );
    let candidate = run(tmp.path(), &["path", "candidates/codex"]);
    assert_eq!(candidate.code, 0, "stderr={}", candidate.stderr_text());
    assert_eq!(
        candidate.stdout_text(),
        format!("{}/candidates/codex\n", tmp.path().display())
    );

    let escape = run(tmp.path(), &["path", "profiles/../../outside"]);
    assert_eq!(escape.code, 64);
}

// ---- inactive archive lifecycle ----------------------------------------

fn seed_archive_layout(root: &Path) {
    seed_recall_layout(root);
    fs::write(
        root.join("global/runtime-enforced.md"),
        note(
            "runtime-enforced",
            "feedback",
            "Archive-only historical token.\n\n**Why:** original correction.\n\n**How to apply:** old reminder.",
        ),
    )
    .expect("retirement source");
    fs::write(
        root.join("global/MEMORY.md"),
        "# Memory index\n\n- [Routing](routing.md) — active routing\n- [Runtime enforced](runtime-enforced.md) — superseded reminder\n",
    )
    .expect("global index");
    fs::write(
        root.join("profiles/startup/MEMORY.md"),
        "# Startup\n\n- [Runtime enforced](../../global/runtime-enforced.md) — superseded reminder\n",
    )
    .expect("startup index");
}

#[test]
fn archive_retire_is_dry_run_first_then_moves_note_out_of_active_recall() {
    let tmp = tempfile::TempDir::new().expect("tempdir");
    seed_archive_layout(tmp.path());
    let args = [
        "archive",
        "retire",
        "runtime-enforced",
        "--reason",
        "enforced-by-runtime",
        "--superseded-by",
        "nils-cli:crates/example/src/lib.rs",
        "--archived-at",
        "2026-07-12",
        "--format",
        "json",
    ];

    let preview = run(tmp.path(), &args);
    assert_eq!(preview.code, 0, "stderr={}", preview.stderr_text());
    let preview_json: serde_json::Value =
        serde_json::from_str(preview.stdout_text().trim()).expect("archive preview json");
    assert_eq!(
        preview_json["schema_version"],
        "cli.agent-memory.archive-retire.v1"
    );
    assert_eq!(preview_json["mode"], "dry-run");
    assert_eq!(
        preview_json["active_index_updates"]
            .as_array()
            .unwrap()
            .len(),
        2
    );
    assert!(tmp.path().join("global/runtime-enforced.md").is_file());
    assert!(
        !tmp.path()
            .join("archive/superseded/runtime-enforced.md")
            .exists()
    );

    let mut apply_args = args.to_vec();
    apply_args.push("--apply");
    let applied = run(tmp.path(), &apply_args);
    assert_eq!(applied.code, 0, "stderr={}", applied.stderr_text());
    assert!(!tmp.path().join("global/runtime-enforced.md").exists());
    let archived = fs::read_to_string(tmp.path().join("archive/superseded/runtime-enforced.md"))
        .expect("archived note");
    assert!(archived.contains("lifecycleStatus: archived"), "{archived}");
    assert!(
        archived.contains("archiveReason: \"enforced-by-runtime\""),
        "{archived}"
    );
    assert!(
        archived.contains("nils-cli:crates/example/src/lib.rs"),
        "{archived}"
    );
    assert!(
        !fs::read_to_string(tmp.path().join("global/MEMORY.md"))
            .expect("global index")
            .contains("runtime-enforced.md")
    );
    assert!(
        !fs::read_to_string(tmp.path().join("profiles/startup/MEMORY.md"))
            .expect("startup index")
            .contains("runtime-enforced.md")
    );

    let active = run(
        tmp.path(),
        &["recall", "on-demand", "Archive-only historical token"],
    );
    assert_eq!(active.code, 1);
    let active_all = run(
        tmp.path(),
        &["search", "Archive-only historical token", "--all"],
    );
    assert_eq!(active_all.code, 1);
    let archive_list = run(tmp.path(), &["archive", "list", "--format", "json"]);
    assert_eq!(
        archive_list.code,
        0,
        "stderr={}",
        archive_list.stderr_text()
    );
    let list_json: serde_json::Value =
        serde_json::from_str(archive_list.stdout_text().trim()).expect("archive list json");
    assert_eq!(list_json["count"], 1);
    assert_eq!(list_json["notes"][0]["file"], "runtime-enforced.md");
    let historical = run(
        tmp.path(),
        &["archive", "search", "Archive-only historical token"],
    );
    assert_eq!(historical.code, 0, "stderr={}", historical.stderr_text());
    assert!(historical.stdout_text().contains("runtime-enforced.md"));
}

#[test]
fn archive_retire_reports_active_inbound_references_without_writes() {
    let tmp = tempfile::TempDir::new().expect("tempdir");
    seed_archive_layout(tmp.path());
    fs::write(
        tmp.path().join("global/routing.md"),
        note(
            "routing",
            "reference",
            "Still points at [[runtime-enforced]].",
        ),
    )
    .expect("inbound note");

    let out = run(
        tmp.path(),
        &[
            "archive",
            "retire",
            "runtime-enforced",
            "--reason",
            "enforced-by-runtime",
            "--superseded-by",
            "agent-runtime-kit:core/policies/example.md",
            "--archived-at",
            "2026-07-12",
            "--apply",
            "--format",
            "json",
        ],
    );
    assert_eq!(out.code, 1);
    assert!(out.stderr_text().is_empty());
    let doc: serde_json::Value =
        serde_json::from_str(out.stdout_text().trim()).expect("archive blocker json");
    assert_eq!(doc["error"]["code"], "active-reference-blocked");
    assert_eq!(doc["blockers"][0]["file"], "global/routing.md");
    assert!(tmp.path().join("global/runtime-enforced.md").is_file());
    assert!(
        !tmp.path()
            .join("archive/superseded/runtime-enforced.md")
            .exists()
    );
}

#[test]
fn archive_is_explicit_history_not_a_normal_memory_scope() {
    let tmp = tempfile::TempDir::new().expect("tempdir");
    seed_archive_layout(tmp.path());

    let scope = run(tmp.path(), &["path", "archive/superseded"]);
    assert_eq!(scope.code, 64);

    let list_before = run(tmp.path(), &["archive", "list", "--format", "json"]);
    assert_eq!(list_before.code, 0, "stderr={}", list_before.stderr_text());
    let before: serde_json::Value =
        serde_json::from_str(list_before.stdout_text().trim()).expect("archive list json");
    assert_eq!(before["count"], 0);
}

#[test]
fn archive_retire_rejects_duplicate_targets_and_symlink_roots_without_writes() {
    let tmp = tempfile::TempDir::new().expect("tempdir");
    seed_archive_layout(tmp.path());
    fs::create_dir_all(tmp.path().join("archive/superseded")).expect("archive");
    fs::write(
        tmp.path().join("archive/superseded/runtime-enforced.md"),
        "existing history",
    )
    .expect("existing archive");
    let base = [
        "archive",
        "retire",
        "runtime-enforced",
        "--reason",
        "enforced-by-runtime",
        "--superseded-by",
        "nils-cli:crates/example/src/lib.rs",
        "--archived-at",
        "2026-07-12",
        "--apply",
    ];
    let duplicate = run(tmp.path(), &base);
    assert_eq!(duplicate.code, 1);
    assert!(duplicate.stderr_text().contains("already exists"));
    assert!(tmp.path().join("global/runtime-enforced.md").is_file());

    #[cfg(unix)]
    {
        fs::remove_dir_all(tmp.path().join("archive")).expect("remove archive");
        std::os::unix::fs::symlink(tmp.path().join("global"), tmp.path().join("archive"))
            .expect("archive symlink");
        let linked = run(tmp.path(), &base);
        assert_eq!(linked.code, 1);
        assert!(linked.stderr_text().contains("must not be a symlink"));
        assert!(tmp.path().join("global/runtime-enforced.md").is_file());

        let list = run(tmp.path(), &["archive", "list"]);
        assert_eq!(list.code, 1);
        assert!(
            list.stderr_text()
                .contains("regular, non-symlink directory")
        );

        let search = run(tmp.path(), &["archive", "search", "token"]);
        assert_eq!(search.code, 1);
        assert!(
            search
                .stderr_text()
                .contains("regular, non-symlink directory")
        );
    }
}

#[test]
fn archive_retire_rejects_malformed_metadata_multiline_reason_and_stale_recovery() {
    let tmp = tempfile::TempDir::new().expect("tempdir");
    seed_archive_layout(tmp.path());
    let source = tmp.path().join("global/runtime-enforced.md");
    fs::write(
        &source,
        "---\nname: runtime-enforced\ndescription: missing metadata\n---\n\nBody.\n",
    )
    .expect("malformed note");
    let base = [
        "archive",
        "retire",
        "runtime-enforced",
        "--reason",
        "enforced-by-runtime",
        "--superseded-by",
        "nils-cli:crates/example/src/lib.rs",
        "--archived-at",
        "2026-07-12",
        "--apply",
    ];
    let malformed = run(tmp.path(), &base);
    assert_eq!(malformed.code, 1);
    assert!(
        malformed
            .stderr_text()
            .contains("incomplete memory frontmatter")
    );

    fs::write(
        &source,
        note(
            "runtime-enforced",
            "feedback",
            "Restored valid source.\n\n**Why:** reason.\n\n**How to apply:** old reminder.",
        ),
    )
    .expect("valid note");
    let multiline = run(
        tmp.path(),
        &[
            "archive",
            "retire",
            "runtime-enforced",
            "--reason",
            "line one\nline two",
            "--superseded-by",
            "nils-cli:crates/example/src/lib.rs",
            "--archived-at",
            "2026-07-12",
        ],
    );
    assert_eq!(multiline.code, 64);
    assert!(multiline.stderr_text().contains("single line"));

    let recovery = tmp
        .path()
        .join("global/.runtime-enforced.md.archive-backup");
    fs::write(&recovery, "previous recovery").expect("recovery");
    let stale = run(tmp.path(), &base);
    assert_eq!(stale.code, 1);
    assert!(
        stale
            .stderr_text()
            .contains("stale archive transaction path")
    );
    assert_eq!(
        fs::read_to_string(recovery).expect("retained recovery"),
        "previous recovery"
    );
    assert!(source.is_file());
    assert!(!tmp.path().join("archive").exists());
}

// ---- strict index budget and forbidden-term audits ----------------------

#[test]
fn check_enforces_index_byte_budget_in_text_and_json() {
    let tmp = tempfile::TempDir::new().expect("tempdir");
    seed_check_scope(tmp.path());

    let text = run(tmp.path(), &["check", "global", "--max-index-bytes", "8"]);
    assert_eq!(text.code, 1);
    assert!(text.stdout_text().contains("index-byte-budget-exceeded"));
    assert!(text.stdout_text().contains("maximum is 8 bytes"));

    let json = run(
        tmp.path(),
        &[
            "check",
            "global",
            "--max-index-bytes",
            "8",
            "--format",
            "json",
        ],
    );
    assert_eq!(json.code, 1);
    assert!(json.stdout_text().contains("index-byte-budget-exceeded"));
}

#[test]
fn check_reports_forbidden_terms_with_file_line_and_term() {
    let tmp = tempfile::TempDir::new().expect("tempdir");
    seed_check_scope(tmp.path());
    let terms = tmp.path().join("retired-terms.txt");
    fs::write(&terms, "retired.skill\ncore/skills/retired/path\n").expect("terms");
    fs::write(
        tmp.path().join("global/alpha.md"),
        note("alpha-note", "user", "Uses retired.skill by mistake."),
    )
    .expect("alpha");

    let out = run(
        tmp.path(),
        &[
            "check",
            "global",
            "--forbid-terms-file",
            &terms.to_string_lossy(),
            "--format",
            "json",
        ],
    );
    assert_eq!(out.code, 1, "stderr={}", out.stderr_text());
    let stdout = out.stdout_text();
    assert!(stdout.contains("\"kind\":\"forbidden-term\""), "{stdout}");
    assert!(stdout.contains("\"file\":\"alpha.md\""), "{stdout}");
    assert!(stdout.contains("retired.skill"), "{stdout}");
    assert!(stdout.contains("line 10"), "{stdout}");
}

#[test]
fn check_rejects_empty_or_symlink_forbidden_term_files() {
    let tmp = tempfile::TempDir::new().expect("tempdir");
    seed_check_scope(tmp.path());
    let empty = tmp.path().join("empty.txt");
    fs::write(&empty, "# comments only\n\n").expect("empty terms");

    let empty_out = run(
        tmp.path(),
        &[
            "check",
            "global",
            "--forbid-terms-file",
            &empty.to_string_lossy(),
        ],
    );
    assert_eq!(empty_out.code, 64);
    assert!(empty_out.stderr_text().contains("contains no terms"));

    #[cfg(unix)]
    {
        let linked = tmp.path().join("linked.txt");
        std::os::unix::fs::symlink(&empty, &linked).expect("terms symlink");
        let linked_out = run(
            tmp.path(),
            &[
                "check",
                "global",
                "--forbid-terms-file",
                &linked.to_string_lossy(),
            ],
        );
        assert_eq!(linked_out.code, 1);
        assert!(linked_out.stderr_text().contains("symlink"));
    }
}

#[cfg(unix)]
#[test]
fn check_rejects_indexed_note_symlinks() {
    let tmp = tempfile::TempDir::new().expect("tempdir");
    seed_check_scope(tmp.path());
    fs::remove_file(tmp.path().join("global/alpha.md")).expect("remove alpha");
    std::os::unix::fs::symlink(
        tmp.path().join("global/beta.md"),
        tmp.path().join("global/alpha.md"),
    )
    .expect("note symlink");

    let out = run(tmp.path(), &["check", "global"]);
    assert_eq!(out.code, 1);
    assert!(out.stdout_text().contains("index-unsafe-link"));
}

//! Task 3.3 — `init` emits an annotated override stub: valid, declares no
//! required entries by default, and embeds the schema + `when` grammar.

use super::common::TestEnv;

#[test]
fn init_print_is_valid_and_declares_nothing() {
    let env = TestEnv::new();
    let out = env.run(&["init", "--print"]);
    assert!(out.success(), "stderr: {}", out.stderr);
    let stub = out.stdout;

    // The stub embeds schema + grammar guidance.
    assert!(
        stub.contains("[[document]]"),
        "stub missing schema:\n{stub}"
    );
    assert!(
        stub.contains("[[validation]]"),
        "stub missing validation schema:\n{stub}"
    );
    assert!(
        stub.contains("path-exists:"),
        "stub missing when grammar:\n{stub}"
    );

    // It declares no *active* entries: every schema line is commented out, so
    // using the stub verbatim as a project catalog adds zero requirements.
    let fresh = TestEnv::new();
    fresh.write_project_catalog(&stub);
    let pf = fresh.run(&["preflight", "--intent", "project-dev", "--format", "json"]);
    assert!(
        pf.success(),
        "stub should be a valid empty catalog: {}",
        pf.stderr
    );
    assert_eq!(
        pf.json()["documents"].as_array().unwrap().len(),
        0,
        "stub should declare no documents:\n{stub}"
    );
}

#[test]
fn init_dry_run_does_not_write() {
    let env = TestEnv::new();
    let out = env.run(&["init", "--dry-run"]);
    assert!(out.success(), "stderr: {}", out.stderr);
    assert!(
        !env.project_path("AGENT_DOCS.toml").exists(),
        "--dry-run must not write the file"
    );
}

#[test]
fn init_force_writes_the_stub() {
    let env = TestEnv::new();
    let out = env.run(&["init", "--force"]);
    assert!(out.success(), "stderr: {}", out.stderr);
    let written = env.project_path("AGENT_DOCS.toml");
    assert!(written.exists(), "--force should write AGENT_DOCS.toml");
    let body = std::fs::read_to_string(&written).unwrap();
    assert!(
        body.contains("project-local override"),
        "unexpected stub:\n{body}"
    );
}

#[test]
fn init_prefills_rust_example_when_cargo_toml_present() {
    let env = TestEnv::new();
    env.write_project_doc("Cargo.toml", "[package]\nname = \"x\"\n");
    let out = env.run(&["init", "--print"]);
    assert!(out.success(), "stderr: {}", out.stderr);
    assert!(
        out.stdout.contains("Detected Cargo.toml"),
        "expected a Rust example pre-fill:\n{}",
        out.stdout
    );
}

#[test]
fn init_inherited_comments_preserve_product_scope() {
    let env = TestEnv::new();
    env.write_home_catalog(
        "[[document]]\ncontext = \"project-dev\"\nscope = \"home\"\npath = \"CODEX.md\"\nproduct = \"codex\"\n\n[[validation]]\ncontext = \"project-dev\"\ncommands = [\"codex-check\"]\nproduct = [\"codex\", \"claude\"]\n",
    );

    let out = env.run(&["init", "--print"]);
    assert!(out.success(), "stderr: {}", out.stderr);
    assert!(
        out.stdout
            .contains("document: context=project-dev scope=home path=CODEX.md product=[\"codex\"]"),
        "{}",
        out.stdout
    );
    assert!(
        out.stdout
            .contains("validation: context=project-dev product=[\"codex\", \"claude\"]"),
        "{}",
        out.stdout
    );
}

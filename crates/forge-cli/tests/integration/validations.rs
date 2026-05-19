//! End-to-end tests asserting that validation rules return the right
//! `error.kind` discriminator when wired through the public surface.
//!
//! The pr-create / pr-edit integration suites (Tasks 2.2 / 2.4) exercise
//! these rules from the CLI boundary. This module pins the contract at the
//! library boundary instead: it is the canonical "validation `error.kind`
//! catalog" reference that downstream sprints can grep when wiring new
//! atoms.

use forge_cli::validations::{
    BodyHeadings, BranchPrefix, HeadState, PrKind, body_summary, body_test_plan,
    branch_kind_matches, branch_name, head_pushed, title_length, worktree_clean,
};

#[test]
fn validation_kinds_match_spec_catalog() {
    // Each row encodes the spec §"Lock-down policy" mapping. If any
    // discriminator drifts, this test fails so the spec / code pair stays
    // honest.
    let cases: &[(&str, &str)] = &[
        ("branch_name (invalid)", "branch_name_invalid"),
        ("branch_kind_matches (mismatch)", "branch_kind_mismatch"),
        ("title_length (over cap)", "title_too_long"),
        ("body_summary (missing)", "body_missing_summary"),
        ("body_test_plan (missing)", "body_missing_test_plan"),
        ("worktree_clean (dirty)", "dirty_worktree"),
        ("head_pushed (no upstream)", "head_not_pushed"),
    ];

    let errs: Vec<&'static str> = vec![
        branch_name("docs/release").unwrap_err().kind(),
        branch_kind_matches(BranchPrefix::Feat, PrKind::Bug)
            .unwrap_err()
            .kind(),
        title_length(&"a".repeat(71)).unwrap_err().kind(),
        body_summary("## Test plan\n\ntext\n", &BodyHeadings::default())
            .unwrap_err()
            .kind(),
        body_test_plan("## Summary\n\nx\n", &BodyHeadings::default())
            .unwrap_err()
            .kind(),
        worktree_clean(std::path::Path::new("."), |_| Ok(" M f.rs\n".into()))
            .unwrap_err()
            .kind(),
        head_pushed(std::path::Path::new("."), |_| {
            Ok(HeadState {
                head_sha: "abc".into(),
                upstream_sha: None,
            })
        })
        .unwrap_err()
        .kind(),
    ];

    for ((label, expected), got) in cases.iter().zip(errs.iter()) {
        assert_eq!(*got, *expected, "case={label}");
    }
}

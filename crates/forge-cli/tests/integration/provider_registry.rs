use std::fs;

use super::support::{StubEnv, parse_envelope, run_forge_cli};

#[test]
fn provider_add_list_and_view_persist_only_a_token_reference() {
    let stub = StubEnv::new().env("FORGEJO_TEST_TOKEN", "must-not-be-persisted");

    let added = run_forge_cli(
        &stub,
        &[
            "--format",
            "json",
            "provider",
            "add",
            "codeberg",
            "--kind",
            "forgejo",
            "--base-url",
            "https://codeberg.org",
            "--token-env",
            "FORGEJO_TEST_TOKEN",
        ],
    );
    assert_eq!(added.code, 0, "stderr={}", added.stderr);
    let added_json = parse_envelope(&added.stdout);
    assert_eq!(
        added_json["schema_version"],
        "cli.forge-cli.provider.add.v1"
    );
    assert_eq!(added_json["data"]["name"], "codeberg");
    assert_eq!(added_json["data"]["kind"], "forgejo");
    assert_eq!(added_json["data"]["base_url"], "https://codeberg.org");
    assert_eq!(added_json["data"]["token_env"], "FORGEJO_TEST_TOKEN");

    let registry = stub
        .tempdir
        .path()
        .join("xdg-config/forge-cli/providers.toml");
    let persisted = fs::read_to_string(&registry).expect("provider registry");
    assert!(persisted.contains("FORGEJO_TEST_TOKEN"));
    assert!(!persisted.contains("must-not-be-persisted"));
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(
            fs::metadata(&registry).unwrap().permissions().mode() & 0o777,
            0o600
        );
    }

    let listed = run_forge_cli(&stub, &["--format", "json", "provider", "list"]);
    assert_eq!(listed.code, 0, "stderr={}", listed.stderr);
    let listed_json = parse_envelope(&listed.stdout);
    assert_eq!(
        listed_json["schema_version"],
        "cli.forge-cli.provider.list.v1"
    );
    assert_eq!(listed_json["data"]["providers"][0]["name"], "codeberg");
    assert!(!listed.stdout.contains("must-not-be-persisted"));

    let viewed = run_forge_cli(&stub, &["--format", "json", "provider", "view", "codeberg"]);
    assert_eq!(viewed.code, 0, "stderr={}", viewed.stderr);
    let viewed_json = parse_envelope(&viewed.stdout);
    assert_eq!(
        viewed_json["schema_version"],
        "cli.forge-cli.provider.view.v1"
    );
    assert_eq!(viewed_json["data"]["name"], "codeberg");
    assert!(!viewed.stdout.contains("must-not-be-persisted"));

    let selected = run_forge_cli(
        &stub,
        &[
            "operation-effect",
            "--format",
            "json",
            "--",
            "--provider",
            "codeberg",
            "auth",
            "status",
        ],
    );
    assert_eq!(selected.code, 0, "stderr={}", selected.stderr);
    assert_eq!(
        parse_envelope(&selected.stdout)["data"]["operation"],
        "auth.status"
    );
}

#[test]
fn provider_add_rejects_reserved_names_and_unsafe_base_urls() {
    let stub = StubEnv::new();
    for (name, base_url, expected) in [
        ("github", "https://codeberg.org", "provider_name_reserved"),
        (
            "codeberg",
            "https://user@example.com",
            "provider_base_url_invalid",
        ),
        (
            "codeberg",
            "http://example.com",
            "provider_base_url_invalid",
        ),
    ] {
        let output = run_forge_cli(
            &stub,
            &[
                "--format",
                "json",
                "provider",
                "add",
                name,
                "--kind",
                "forgejo",
                "--base-url",
                base_url,
                "--token-env",
                "FORGEJO_TEST_TOKEN",
            ],
        );
        assert_eq!(
            output.code, 65,
            "stdout={} stderr={}",
            output.stdout, output.stderr
        );
        assert_eq!(parse_envelope(&output.stdout)["error"]["code"], expected);
    }
}

#[test]
fn built_in_provider_names_remain_valid_without_registry_records() {
    let stub = StubEnv::new();
    for provider in ["github", "gitlab", "local"] {
        let output = run_forge_cli(
            &stub,
            &[
                "operation-effect",
                "--format",
                "json",
                "--",
                "--provider",
                provider,
                "auth",
                "status",
            ],
        );
        assert_eq!(
            output.code, 0,
            "provider={provider}; stderr={}",
            output.stderr
        );
        assert_eq!(
            parse_envelope(&output.stdout)["data"]["operation"],
            "auth.status"
        );
    }
}

use std::fs;
use std::path::Path;

use nils_test_support::fixtures::{
    GraphqlSetupFixture, RestSetupFixture, SuiteFixture, write_text,
};

#[test]
fn rest_setup_fixture_writes_expected_files() {
    let fixture = RestSetupFixture::new();
    let endpoints = fixture.write_endpoints_env("API_URL=http://localhost\n");
    let endpoints_local = fixture.write_endpoints_local_env("API_URL=http://local\n");
    let tokens = fixture.write_tokens_env("TOKEN=abc\n");
    let tokens_local = fixture.write_tokens_local_env("TOKEN=def\n");

    assert!(fixture.setup_dir.ends_with(Path::new("setup/rest")));
    assert_eq!(
        fs::read_to_string(endpoints).unwrap(),
        "API_URL=http://localhost\n"
    );
    assert_eq!(
        fs::read_to_string(endpoints_local).unwrap(),
        "API_URL=http://local\n"
    );
    assert_eq!(fs::read_to_string(tokens).unwrap(), "TOKEN=abc\n");
    assert_eq!(fs::read_to_string(tokens_local).unwrap(), "TOKEN=def\n");
}

#[test]
fn graphql_setup_fixture_writes_expected_files() {
    let fixture = GraphqlSetupFixture::new();
    let endpoints = fixture.write_endpoints_env("API_URL=http://localhost\n");
    let endpoints_local = fixture.write_endpoints_local_env("API_URL=http://local\n");
    let jwts = fixture.write_jwts_env("JWT=abc\n");
    let jwts_local = fixture.write_jwts_local_env("JWT=def\n");
    let schema_env = fixture.write_schema_env("SCHEMA_FILE=schema.graphql\n");
    let schema_file = fixture.write_schema_file("schema.graphql", "type Query { ok: Boolean }\n");

    assert!(fixture.setup_dir.ends_with(Path::new("setup/graphql")));
    assert_eq!(
        fs::read_to_string(endpoints).unwrap(),
        "API_URL=http://localhost\n"
    );
    assert_eq!(
        fs::read_to_string(endpoints_local).unwrap(),
        "API_URL=http://local\n"
    );
    assert_eq!(fs::read_to_string(jwts).unwrap(), "JWT=abc\n");
    assert_eq!(fs::read_to_string(jwts_local).unwrap(), "JWT=def\n");
    assert_eq!(
        fs::read_to_string(schema_env).unwrap(),
        "SCHEMA_FILE=schema.graphql\n"
    );
    assert_eq!(
        fs::read_to_string(schema_file).unwrap(),
        "type Query { ok: Boolean }\n"
    );
}

#[test]
fn suite_fixture_writes_minimal_suites_and_supporting_files() {
    let fixture = SuiteFixture::new();
    let suite_path =
        fixture.write_minimal_rest_suite("rest.health", "setup/rest/requests/health.request.json");
    let manifest = fs::read_to_string(&suite_path).unwrap();
    assert!(manifest.contains("\"rest.health\""));
    assert!(manifest.contains("health.request.json"));

    let suite_path =
        fixture.write_minimal_graphql_suite("graphql.health", "setup/graphql/ops/health.graphql");
    let manifest = fs::read_to_string(&suite_path).unwrap();
    assert!(manifest.contains("\"graphql.health\""));
    assert!(manifest.contains("health.graphql"));

    let request_path = fixture.root.join("setup/rest/requests/health.request.json");
    assert!(request_path.exists());
    let op_path = fixture.root.join("setup/graphql/ops/health.graphql");
    assert!(op_path.exists());
}

#[test]
fn write_text_creates_parent_dirs() {
    let temp = tempfile::TempDir::new().unwrap();
    let path = temp.path().join("nested/dir/file.txt");
    let written = write_text(&path, "hello\n");
    assert_eq!(fs::read_to_string(written).unwrap(), "hello\n");
}

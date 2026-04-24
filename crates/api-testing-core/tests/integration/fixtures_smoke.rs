use crate::support;
use support::RepoFixture;

#[test]
fn fixtures_smoke_builds_repo_layout() {
    let repo = RepoFixture::new();
    repo.write_rest_endpoints("REST_URL_STAGING=http://localhost:1234\n");
    repo.write_rest_tokens("REST_TOKEN_SERVICE=token\n");
    repo.write_gql_endpoints("GQL_URL_STAGING=http://localhost:5678/graphql\n");
    repo.write_gql_jwts("GQL_JWT_SERVICE=jwt\n");
    repo.write_gql_schema_env("GQL_SCHEMA_FILE=schema.graphql\n");
    repo.write_gql_schema_file("schema.graphql", "type Query { ok: Boolean }\n");

    assert!(repo.rest_setup.join("endpoints.env").is_file());
    assert!(repo.rest_setup.join("tokens.env").is_file());
    assert!(repo.gql_setup.join("endpoints.env").is_file());
    assert!(repo.gql_setup.join("jwts.env").is_file());
    assert!(repo.gql_setup.join("schema.env").is_file());
    assert!(repo.gql_setup.join("schema.graphql").is_file());
}

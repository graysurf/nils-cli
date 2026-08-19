use agent_hook::policy_parity::{DSH_CAPABILITY_GROUP_SCHEMA_VERSION, DshCapabilityGroup};
use pretty_assertions::assert_eq;
use serde::Deserialize;

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct Fixture {
    schema_version: String,
    capabilities: Vec<CapabilityFixture>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct CapabilityFixture {
    id: DshCapabilityGroup,
    migration_task: String,
}

#[test]
fn dsh_capability_group_schema_matches_the_frozen_migration_fixture() {
    let fixture: Fixture = serde_json::from_str(include_str!(
        "fixtures/dsh-policy-capability-groups.v1.json"
    ))
    .expect("valid DSH capability group fixture");

    assert_eq!(fixture.schema_version, DSH_CAPABILITY_GROUP_SCHEMA_VERSION);
    assert_eq!(fixture.capabilities.len(), 23);
    assert_eq!(
        fixture
            .capabilities
            .iter()
            .map(|entry| entry.id)
            .collect::<Vec<_>>(),
        DshCapabilityGroup::ALL,
    );
    assert!(
        fixture
            .capabilities
            .iter()
            .all(|entry| matches!(entry.migration_task.as_str(), "2.3" | "3.2" | "3.3" | "3.4"))
    );
}

#[test]
fn dsh_capability_group_schema_rejects_unknown_ids() {
    assert!(serde_json::from_str::<DshCapabilityGroup>("\"unknown-python-handler\"").is_err());
}

#[test]
fn canonical_spec_names_the_strict_dsh_ingress_and_policy_contracts() {
    let specification = include_str!("../docs/specs/agent-hook-v1.md");
    for contract in [
        "`agent-hook.dsh-ingress.v1`",
        "`agent-hook.dsh-ingress.v2`",
        "`agent-hook.dsh-ingress.v3`",
        "`agent-hook.dsh-ingress.v4`",
        "`dsh.policy.v1`",
    ] {
        assert!(
            specification.contains(contract),
            "canonical specification is missing {contract}"
        );
    }
    assert!(specification.contains("v1 explicitly forbids `subject`"));
    assert!(specification.contains("v2 requires this complete subject"));
    assert!(specification.contains("native allow/block admission"));
    assert!(specification.contains("bounded model"));
}

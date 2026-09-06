use agent_hook::policy_parity::{DSH_CAPABILITY_GROUP_SCHEMA_VERSION, DshCapabilityGroup, DshTier};
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
    tier: DshTier,
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
    for entry in &fixture.capabilities {
        assert_eq!(
            entry.tier,
            entry.id.tier(),
            "{} tier drifted from the frozen table",
            entry.id.as_str()
        );
    }
}

#[test]
fn every_dsh_capability_group_has_exactly_one_tier_with_the_accepted_defaults() {
    let integrity = [
        DshCapabilityGroup::OwnerUnclaimed,
        DshCapabilityGroup::SemanticConflict,
        DshCapabilityGroup::OperationLifecycle,
        DshCapabilityGroup::AgentScopeLockGuard,
        DshCapabilityGroup::CheckoutLeaseGuard,
        DshCapabilityGroup::McpSecretScan,
        DshCapabilityGroup::FinishLineRecord,
    ];
    let governed = [
        DshCapabilityGroup::BlockDirectGitCommit,
        DshCapabilityGroup::BlockDirectGitWorktree,
        DshCapabilityGroup::BlockDirectPrCreate,
        DshCapabilityGroup::BlockUnsafeDefaultDelivery,
        DshCapabilityGroup::SemanticCommitBodyGate,
        DshCapabilityGroup::BlockProjectMemoryWrite,
        DshCapabilityGroup::PortablePathsScan,
        DshCapabilityGroup::PreEditIntentGate,
    ];
    let reminders = [
        DshCapabilityGroup::ForgeLabelReminder,
        DshCapabilityGroup::MemoryWritePrincipleReminder,
        DshCapabilityGroup::SkillUsageReminder,
        DshCapabilityGroup::StopPrePrReminder,
        DshCapabilityGroup::UserPromptAgentMemory,
        DshCapabilityGroup::SessionStartHealthcheck,
        DshCapabilityGroup::AgentActivity,
        DshCapabilityGroup::BlockDirectPython,
    ];
    assert_eq!(
        integrity.len() + governed.len() + reminders.len(),
        DshCapabilityGroup::ALL.len()
    );
    for group in DshCapabilityGroup::ALL {
        let expected = if integrity.contains(&group) {
            DshTier::Integrity
        } else if governed.contains(&group) {
            DshTier::GovernedSeam
        } else {
            assert!(reminders.contains(&group), "{} is untiered", group.as_str());
            DshTier::Reminder
        };
        assert_eq!(group.tier(), expected, "{}", group.as_str());
        assert_eq!(
            group.remediation().is_some(),
            group.tier() == DshTier::GovernedSeam,
            "{}: exactly the Tier B seams carry remediation",
            group.as_str()
        );
        assert!(
            group.shell_subjects().is_empty() || group.tier() == DshTier::GovernedSeam,
            "{}: only Tier B groups narrow the shell classification by subject",
            group.as_str()
        );
    }
    assert_eq!(DshTier::Integrity.enforcement_default(), "block");
    assert_eq!(DshTier::GovernedSeam.enforcement_default(), "block");
    assert_eq!(DshTier::Reminder.enforcement_default(), "context");
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
        "`agent-hook.dsh-ingress.v5`",
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

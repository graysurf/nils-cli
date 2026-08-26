use std::collections::{BTreeMap, BTreeSet};
use std::fs::OpenOptions;
use std::io::Read;
use std::os::unix::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use serde_json::json;

use super::{
    CompletionDisposition, ContractSnapshot, HookError, IdentityInput, NormalizedOutcome, Outcome,
    RequestIdentity, Store, StoredOperationKind, compact_obsolete_sessions, constant_time_eq,
    digest_parts, finish_line_temporary, finish_line_unavailable, operation_key, parse_request,
    resolve_contracts, runner_capability_digest, success_outcome, validate_command,
    validate_identifier, validate_identity, validate_intent, validate_runner_capability,
    verify_private_regular, write_state_atomic,
};

const STATE_SCHEMA: &str = "agent-hook.finish-line.acceptance-state.v1";
const STATE_MAX_BYTES: u64 = 384 * 1024;
const MAX_REQUIREMENTS: usize = 128;
const MAX_VALIDATORS_PER_REQUIREMENT: usize = 16;
const MAX_INVALIDATORS: usize = 128;
const MAX_OPERATIONS: usize = 512;
const COMPACTION_TRIGGER_OPERATIONS: usize = 256;
const COMPACTED_OPERATION_COUNT: usize = 192;

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct RegisterRequest {
    schema_version: String,
    product: String,
    session_id: String,
    turn_id: String,
    cwd: PathBuf,
    runner_capability: String,
    requirements: Vec<RequirementRegistration>,
    invalidators: Vec<DefinitionRegistration>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct RequirementRegistration {
    name: String,
    validators: Vec<ValidatorRegistration>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct ValidatorRegistration {
    id: String,
    tool_name: String,
    definition_digest: String,
    execution: ValidatorExecutionRegistration,
}

#[derive(Debug, Deserialize)]
#[serde(tag = "kind", rename_all = "kebab-case", deny_unknown_fields)]
enum ValidatorExecutionRegistration {
    HostObserved,
    ContainedBash { intent: String, command: String },
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct DefinitionRegistration {
    tool_name: String,
    definition_digest: String,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct AdmitRequest {
    schema_version: String,
    product: String,
    session_id: String,
    turn_id: String,
    cwd: PathBuf,
    runner_capability: String,
    contract_digest: String,
    operation_id: String,
    attempt_token: String,
    operation: AdmitOperation,
}

#[derive(Debug, Deserialize)]
#[serde(tag = "kind", rename_all = "kebab-case", deny_unknown_fields)]
enum AdmitOperation {
    Mutation {
        tool_name: String,
        definition_digest: String,
    },
    Validator {
        requirement: String,
        validator_id: String,
        tool_name: String,
        definition_digest: String,
        #[serde(default)]
        source_operation_id: Option<String>,
    },
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct ObserveRequest {
    schema_version: String,
    product: String,
    session_id: String,
    turn_id: String,
    cwd: PathBuf,
    runner_capability: String,
    operation_id: String,
    observation: Observation,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct VerdictRequest {
    schema_version: String,
    product: String,
    session_id: String,
    turn_id: String,
    cwd: PathBuf,
    runner_capability: String,
    contract_digest: String,
    #[serde(default)]
    completion_reservation: Option<CompletionReservationRequest>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct CompletionReservationRequest {
    operation_id: String,
}

macro_rules! identity_input {
    ($type:ty) => {
        impl IdentityInput for $type {
            fn schema_version(&self) -> &str {
                &self.schema_version
            }

            fn product(&self) -> &str {
                &self.product
            }

            fn session_id(&self) -> &str {
                &self.session_id
            }

            fn turn_id(&self) -> &str {
                &self.turn_id
            }

            fn cwd(&self) -> &Path {
                &self.cwd
            }
        }
    };
}

identity_input!(RegisterRequest);
identity_input!(AdmitRequest);
identity_input!(ObserveRequest);
identity_input!(VerdictRequest);

#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct AcceptanceState {
    schema_version: String,
    repo_digest: String,
    next_sequence: u64,
    sessions: BTreeMap<String, AcceptanceSession>,
    operations: BTreeMap<String, AcceptanceOperation>,
}

impl AcceptanceState {
    fn new(repo_digest: &str) -> Self {
        Self {
            schema_version: STATE_SCHEMA.to_string(),
            repo_digest: repo_digest.to_string(),
            next_sequence: 0,
            sessions: BTreeMap::new(),
            operations: BTreeMap::new(),
        }
    }

    fn next_sequence(&mut self) -> Result<u64, HookError> {
        self.next_sequence = self.next_sequence.checked_add(1).ok_or_else(|| {
            HookError::data(
                "finish-line-generation-exhausted",
                "finish-line acceptance sequence is exhausted",
            )
        })?;
        Ok(self.next_sequence)
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
struct AcceptanceContract {
    requirements: BTreeMap<String, RequirementContract>,
    invalidators: BTreeSet<String>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
struct RequirementContract {
    validators: BTreeMap<String, ValidatorContract>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
struct ValidatorContract {
    binding_digest: String,
    execution: ValidatorExecution,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(tag = "kind", rename_all = "kebab-case", deny_unknown_fields)]
enum ValidatorExecution {
    HostObserved,
    ContainedBash {
        intent: String,
        command_digest: String,
        target_digest: String,
        validation_contract_digest: String,
    },
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct AcceptanceSession {
    contract_digest: String,
    contract: AcceptanceContract,
    evidence: BTreeMap<String, RequirementEvidence>,
    #[serde(default)]
    claimed_sources: BTreeSet<String>,
    sequence: u64,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct RequirementEvidence {
    contract_digest: String,
    generation: u64,
    attempt_sequence: u64,
    validator_id: String,
    status: RequirementStatus,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "kebab-case")]
enum RequirementStatus {
    Active,
    Satisfied,
    Failed,
    Uncertain,
    InfrastructureBlocked,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
struct AcceptanceOperation {
    session_key: String,
    turn_key: String,
    token_digest: String,
    capability_digest: String,
    contract_digest: String,
    generation: u64,
    sequence: u64,
    binding_digest: String,
    #[serde(default)]
    source_operation_key: Option<String>,
    kind: AcceptanceOperationKind,
    admission: AdmissionStatus,
    #[serde(default)]
    terminal: Option<AcceptanceTerminal>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(tag = "kind", rename_all = "kebab-case", deny_unknown_fields)]
enum AcceptanceOperationKind {
    Mutation,
    Validator {
        requirement: String,
        validator_id: String,
    },
    Completion,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "kebab-case")]
enum AdmissionStatus {
    Reserved,
    Admitted,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
struct AcceptanceTerminal {
    observation: ObservationStatus,
    source_digest: String,
    disposition: CompletionDisposition,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct Observation {
    kind: ObservationKind,
    #[serde(default)]
    status: Option<ObservationStatus>,
    #[serde(default)]
    operation_id: Option<String>,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq)]
#[serde(rename_all = "kebab-case")]
enum ObservationKind {
    HostObserved,
    ContainedBash,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "kebab-case")]
enum ObservationStatus {
    Succeeded,
    Failed,
    Cancelled,
    TimedOut,
    Signalled,
    Uncertain,
    InfrastructureBlocked,
}

impl ObservationStatus {
    const fn as_str(self) -> &'static str {
        match self {
            Self::Succeeded => "succeeded",
            Self::Failed => "failed",
            Self::Cancelled => "cancelled",
            Self::TimedOut => "timed-out",
            Self::Signalled => "signalled",
            Self::Uncertain => "uncertain",
            Self::InfrastructureBlocked => "infrastructure-blocked",
        }
    }

    const fn requirement_status(self) -> RequirementStatus {
        match self {
            Self::Succeeded => RequirementStatus::Satisfied,
            Self::Failed => RequirementStatus::Failed,
            Self::Cancelled | Self::TimedOut | Self::Signalled | Self::Uncertain => {
                RequirementStatus::Uncertain
            }
            Self::InfrastructureBlocked => RequirementStatus::InfrastructureBlocked,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum VerdictStatus {
    Satisfied,
    Missing,
    Failed,
    Active,
    Uncertain,
    InfrastructureBlocked,
}

impl VerdictStatus {
    const fn as_str(self) -> &'static str {
        match self {
            Self::Satisfied => "satisfied",
            Self::Missing => "missing",
            Self::Failed => "failed",
            Self::Active => "active",
            Self::Uncertain => "uncertain",
            Self::InfrastructureBlocked => "infrastructure-blocked",
        }
    }

    const fn priority(self) -> u8 {
        match self {
            Self::Satisfied => 0,
            Self::Missing => 1,
            Self::Failed => 2,
            Self::Active => 3,
            Self::Uncertain => 4,
            Self::InfrastructureBlocked => 5,
        }
    }
}

pub(super) fn register(state_root: &Path, input: &[u8]) -> Result<Outcome, HookError> {
    let request: RegisterRequest = parse_request(input)?;
    let identity = validate_identity(&request, "agent-hook.finish-line.register.v1")?;
    validate_identifier(&request.runner_capability)?;
    let snapshot = resolve_contracts(&identity)?;
    let contract = normalize_contract(request.requirements, request.invalidators, &snapshot)?;
    let contract_digest = acceptance_contract_digest(&contract)?;

    let store = Store::open(state_root, &identity)?;
    validate_runner_capability(&store, &identity, &request.runner_capability)?;
    let mut state = read_state(&store, &identity)?;
    if terminalize_inactive_completion_reservations(&store, &mut state) {
        save_state(&store, &state)?;
    }
    if let Some(existing) = state.sessions.get(&identity.session_key) {
        if existing.contract_digest == contract_digest && existing.contract == contract {
            let requirement_count = existing.contract.requirements.len();
            let sequence = state.next_sequence()?;
            state
                .sessions
                .get_mut(&identity.session_key)
                .expect("acceptance session checked")
                .sequence = sequence;
            save_state(&store, &state)?;
            return Ok(success_outcome(
                json!({
                    "schema_version": "agent-hook.finish-line.register-result.v1",
                    "status": "duplicate",
                    "contract_digest": contract_digest,
                    "requirement_count": requirement_count,
                    "correlation_id": identity.correlation_id,
                }),
                "finish-line duplicate acceptance contract accepted\n",
            ));
        }
        return Err(HookError::data(
            "finish-line-acceptance-contract-drift",
            "finish-line acceptance contract is immutable for the active DSH session",
        ));
    }
    compact_released_sessions(&mut state, &store.state.sessions);
    if state.sessions.len() >= super::MAX_SESSIONS {
        return Err(HookError::data(
            "finish-line-state-limit",
            "finish-line acceptance session limit is reached",
        ));
    }
    let requirement_count = contract.requirements.len();
    let sequence = state.next_sequence()?;
    state.sessions.insert(
        identity.session_key.clone(),
        AcceptanceSession {
            contract_digest: contract_digest.clone(),
            contract,
            evidence: BTreeMap::new(),
            claimed_sources: BTreeSet::new(),
            sequence,
        },
    );
    save_state(&store, &state)?;
    Ok(success_outcome(
        json!({
            "schema_version": "agent-hook.finish-line.register-result.v1",
            "status": "registered",
            "contract_digest": contract_digest,
            "requirement_count": requirement_count,
            "correlation_id": identity.correlation_id,
        }),
        "finish-line acceptance contract registered\n",
    ))
}

pub(super) fn admit(state_root: &Path, input: &[u8]) -> Result<Outcome, HookError> {
    let request: AdmitRequest = parse_request(input)?;
    let identity = validate_identity(&request, "agent-hook.finish-line.admit.v1")?;
    validate_identifier(&request.runner_capability)?;
    validate_digest(&request.contract_digest)?;
    validate_identifier(&request.operation_id)?;
    validate_identifier(&request.attempt_token)?;

    let operation_key = operation_key(&identity.session_key, &request.operation_id);
    let token_digest = digest_parts(
        "agent-hook.finish-line.acceptance-token.v1",
        &[request.attempt_token.as_bytes()],
    );
    let capability_digest = runner_capability_digest(&identity, &request.runner_capability);
    let (kind, binding_digest) = normalize_admission(&request.operation)?;

    let mut store = Store::open(state_root, &identity)?;
    validate_runner_capability(&store, &identity, &request.runner_capability)?;
    let mut state = read_state(&store, &identity)?;
    if terminalize_inactive_completion_reservations(&store, &mut state) {
        save_state(&store, &state)?;
    }
    let session = acceptance_session(&state, &identity, &request.contract_digest)?;
    let source_operation_key = validate_admission_binding(
        session,
        &kind,
        &binding_digest,
        &request.operation,
        &identity,
    )?;

    if let Some(existing) = state.operations.get(&operation_key).cloned() {
        let exact_retry = existing.session_key == identity.session_key
            && existing.turn_key == identity.turn_key
            && constant_time_eq(existing.token_digest.as_bytes(), token_digest.as_bytes())
            && constant_time_eq(
                existing.capability_digest.as_bytes(),
                capability_digest.as_bytes(),
            )
            && existing.contract_digest == request.contract_digest
            && existing.binding_digest == binding_digest
            && existing.source_operation_key == source_operation_key
            && existing.kind == kind;
        if !exact_retry {
            return Err(HookError::data(
                "finish-line-operation-exists",
                "finish-line acceptance operation_id already has a different binding",
            ));
        }
        if existing.admission == AdmissionStatus::Reserved {
            reconcile_reserved_mutation(&mut store, &mut state, &operation_key, &existing)?;
        }
        return Ok(success_outcome(
            json!({
                "schema_version": "agent-hook.finish-line.admit-result.v1",
                "status": "duplicate",
                "operation_id": request.operation_id,
                "operation_kind": operation_kind_name(&kind),
                "generation": existing.generation,
                "contract_digest": request.contract_digest,
                "correlation_id": identity.correlation_id,
            }),
            "finish-line duplicate acceptance admission accepted\n",
        ));
    }

    if source_operation_key
        .as_ref()
        .is_some_and(|source| store.state.operations.contains_key(source))
    {
        return Err(HookError::data(
            "finish-line-acceptance-source-operation-exists",
            "the contained validation source operation already predates this admission",
        ));
    }

    compact_state(&mut state, store.state.generation);
    if state.operations.len() >= MAX_OPERATIONS {
        return Err(HookError::data(
            "finish-line-state-limit",
            "finish-line acceptance operation limit is reached",
        ));
    }

    match &kind {
        AcceptanceOperationKind::Mutation => {
            let completion_reserved = completion_reservation_active(&store, &state);
            if state.operations.values().any(|operation| {
                matches!(operation.kind, AcceptanceOperationKind::Mutation)
                    && operation.terminal.is_none()
            }) || main_shell_active(&store)
                || completion_reserved
            {
                return Err(finish_line_temporary(
                    if completion_reserved {
                        "finish-line-completion-reserved"
                    } else {
                        "finish-line-acceptance-mutation-active"
                    },
                    "the repository is reserved by an authoritative completion or mutation",
                ));
            }
            let generation = store.state.generation.checked_add(1).ok_or_else(|| {
                HookError::data(
                    "finish-line-generation-exhausted",
                    "finish-line acceptance generation is exhausted",
                )
            })?;
            let sequence = state.next_sequence()?;
            state.operations.insert(
                operation_key.clone(),
                AcceptanceOperation {
                    session_key: identity.session_key.clone(),
                    turn_key: identity.turn_key.clone(),
                    token_digest,
                    capability_digest,
                    contract_digest: request.contract_digest.clone(),
                    generation,
                    sequence,
                    binding_digest,
                    source_operation_key: None,
                    kind: kind.clone(),
                    admission: AdmissionStatus::Reserved,
                    terminal: None,
                },
            );
            save_state(&store, &state)?;
            store.state.generation = generation;
            compact_obsolete_sessions(&mut store.state);
            store.save()?;
            state
                .operations
                .get_mut(&operation_key)
                .expect("inserted acceptance mutation")
                .admission = AdmissionStatus::Admitted;
            save_state(&store, &state)?;
            Ok(admit_outcome(&identity, &request, "mutation", generation))
        }
        AcceptanceOperationKind::Validator {
            requirement,
            validator_id,
        } => {
            let generation = store.state.generation;
            if state.operations.values().any(|operation| {
                matches!(operation.kind, AcceptanceOperationKind::Mutation)
                    && operation.terminal.is_none()
            }) || main_shell_active(&store)
            {
                return Err(finish_line_temporary(
                    "finish-line-acceptance-mutation-active",
                    "a repository mutation has not terminalized",
                ));
            }
            let sequence = state.next_sequence()?;
            if let Some(source) = source_operation_key.as_ref()
                && state
                    .sessions
                    .get(&identity.session_key)
                    .expect("validated acceptance session")
                    .claimed_sources
                    .contains(source)
            {
                return Err(HookError::data(
                    "finish-line-acceptance-source-operation-claimed",
                    "the contained validation source operation is already claimed",
                ));
            }
            state.operations.insert(
                operation_key,
                AcceptanceOperation {
                    session_key: identity.session_key.clone(),
                    turn_key: identity.turn_key.clone(),
                    token_digest,
                    capability_digest,
                    contract_digest: request.contract_digest.clone(),
                    generation,
                    sequence,
                    binding_digest,
                    source_operation_key: source_operation_key.clone(),
                    kind: kind.clone(),
                    admission: AdmissionStatus::Admitted,
                    terminal: None,
                },
            );
            let session = state
                .sessions
                .get_mut(&identity.session_key)
                .expect("validated acceptance session");
            if let Some(source) = source_operation_key {
                session.claimed_sources.insert(source);
            }
            session.evidence.insert(
                requirement.clone(),
                RequirementEvidence {
                    contract_digest: request.contract_digest.clone(),
                    generation,
                    attempt_sequence: sequence,
                    validator_id: validator_id.clone(),
                    status: RequirementStatus::Active,
                },
            );
            save_state(&store, &state)?;
            Ok(admit_outcome(&identity, &request, "validator", generation))
        }
        AcceptanceOperationKind::Completion => unreachable!("completion is reserved by verdict"),
    }
}

pub(super) fn observe(state_root: &Path, input: &[u8]) -> Result<Outcome, HookError> {
    let request: ObserveRequest = parse_request(input)?;
    let identity = validate_identity(&request, "agent-hook.finish-line.observe.v1")?;
    validate_identifier(&request.runner_capability)?;
    validate_identifier(&request.operation_id)?;
    let operation_key = operation_key(&identity.session_key, &request.operation_id);
    let capability_digest = runner_capability_digest(&identity, &request.runner_capability);

    let store = Store::open(state_root, &identity)?;
    validate_runner_capability(&store, &identity, &request.runner_capability)?;
    let mut state = read_state(&store, &identity)?;
    let existing = state
        .operations
        .get(&operation_key)
        .cloned()
        .ok_or_else(|| {
            HookError::data(
                "finish-line-acceptance-operation-missing",
                "finish-line acceptance operation is not registered",
            )
        })?;
    if existing.session_key != identity.session_key
        || !constant_time_eq(
            existing.capability_digest.as_bytes(),
            capability_digest.as_bytes(),
        )
    {
        return Err(HookError::data(
            "finish-line-acceptance-operation-capability-mismatch",
            "finish-line acceptance observation does not own the admitted operation",
        ));
    }
    if existing.admission != AdmissionStatus::Admitted {
        return Err(finish_line_unavailable(
            "finish-line-acceptance-admission-uncertain",
            "finish-line acceptance mutation admission did not durably complete",
        ));
    }
    let execution = operation_execution(&state, &identity, &existing)?;
    let (source_digest, host_observation, contained_operation_key) =
        observation_binding(&identity, &request.observation, execution.as_ref())?;
    if contained_operation_key.is_some()
        && existing.source_operation_key.as_deref() != contained_operation_key.as_deref()
    {
        return Err(HookError::data(
            "finish-line-acceptance-contained-operation-mismatch",
            "finish-line acceptance contained source was not reserved by this validator admission",
        ));
    }
    if let Some(terminal) = existing.terminal {
        let observation_matches =
            host_observation.is_none_or(|observation| terminal.observation == observation);
        if observation_matches && terminal.source_digest == source_digest {
            return Ok(observe_outcome(
                &identity,
                &request,
                "duplicate",
                existing.generation,
                terminal.observation,
            ));
        }
        return Err(HookError::data(
            "finish-line-acceptance-terminal-conflict",
            "finish-line acceptance operation already has a different terminal observation",
        ));
    }
    let observed = match host_observation {
        Some(observation) => observation,
        None => derive_contained_observation(
            &store,
            &existing,
            execution.as_ref().expect("contained execution checked"),
            contained_operation_key
                .as_deref()
                .expect("contained operation binding checked"),
        )?,
    };

    let disposition = match &existing.kind {
        AcceptanceOperationKind::Mutation => {
            if existing.generation == store.state.generation {
                CompletionDisposition::Applied
            } else {
                CompletionDisposition::Stale
            }
        }
        AcceptanceOperationKind::Validator {
            requirement,
            validator_id: _,
        } => {
            if existing.generation != store.state.generation {
                CompletionDisposition::Stale
            } else {
                let evidence = state
                    .sessions
                    .get_mut(&identity.session_key)
                    .and_then(|session| session.evidence.get_mut(requirement));
                if evidence.as_ref().is_none_or(|evidence| {
                    evidence.generation != existing.generation
                        || evidence.contract_digest != existing.contract_digest
                        || evidence.attempt_sequence != existing.sequence
                }) {
                    CompletionDisposition::Superseded
                } else {
                    evidence.expect("acceptance evidence checked").status =
                        observed.requirement_status();
                    CompletionDisposition::Applied
                }
            }
        }
        AcceptanceOperationKind::Completion => {
            if existing.generation == store.state.generation {
                CompletionDisposition::Applied
            } else {
                CompletionDisposition::Stale
            }
        }
    };
    state
        .operations
        .get_mut(&operation_key)
        .expect("acceptance operation checked")
        .terminal = Some(AcceptanceTerminal {
        observation: observed,
        source_digest,
        disposition,
    });
    save_state(&store, &state)?;
    Ok(observe_outcome(
        &identity,
        &request,
        disposition.as_str(),
        existing.generation,
        observed,
    ))
}

pub(super) fn verdict(state_root: &Path, input: &[u8]) -> Result<Outcome, HookError> {
    let request: VerdictRequest = parse_request(input)?;
    let identity = validate_identity(&request, "agent-hook.finish-line.verdict.v1")?;
    validate_identifier(&request.runner_capability)?;
    validate_digest(&request.contract_digest)?;
    if let Some(reservation) = &request.completion_reservation {
        validate_identifier(&reservation.operation_id)?;
    }
    let capability_digest = runner_capability_digest(&identity, &request.runner_capability);
    let snapshot = resolve_contracts(&identity)?;

    let store = Store::open(state_root, &identity)?;
    validate_runner_capability(&store, &identity, &request.runner_capability)?;
    let mut state = read_state(&store, &identity)?;
    if terminalize_inactive_completion_reservations(&store, &mut state) {
        save_state(&store, &state)?;
    }
    let session = acceptance_session(&state, &identity, &request.contract_digest)?;
    let generation = store.state.generation;
    let mut aggregate = VerdictStatus::Satisfied;
    let mut reason_codes = BTreeSet::new();
    let requirements = session
        .contract
        .requirements
        .keys()
        .map(|name| {
            let status = requirement_verdict(
                &state,
                session,
                &identity.session_key,
                name,
                generation,
                &capability_digest,
                &snapshot,
            );
            if status.priority() > aggregate.priority() {
                aggregate = status;
            }
            if status != VerdictStatus::Satisfied {
                reason_codes.insert(status.as_str());
            }
            json!({
                "name": name,
                "status": status.as_str(),
                "attempt_generation": session.evidence.get(name).map(|evidence| evidence.generation),
            })
        })
        .collect::<Vec<_>>();

    for operation in state.operations.values().filter(|operation| {
        matches!(operation.kind, AcceptanceOperationKind::Mutation)
            && mutation_relevant(operation, generation)
    }) {
        let owner_capability_digest = store
            .state
            .sessions
            .get(&operation.session_key)
            .and_then(|session| session.runner_capability_digest.as_deref());
        let status = mutation_verdict(operation, session, generation, owner_capability_digest);
        if status.priority() > aggregate.priority() {
            aggregate = status;
        }
        if status != VerdictStatus::Satisfied {
            reason_codes.insert(status.as_str());
        }
    }

    for operation in store.state.operations.values().filter(|operation| {
        operation.kind == StoredOperationKind::Shell && operation.terminal.is_none()
    }) {
        let status = if operation.generation == generation {
            VerdictStatus::Active
        } else {
            VerdictStatus::InfrastructureBlocked
        };
        if status.priority() > aggregate.priority() {
            aggregate = status;
        }
        reason_codes.insert(status.as_str());
    }

    let action = if aggregate == VerdictStatus::Satisfied {
        "allow"
    } else {
        "block"
    };
    let completion_reservation = if aggregate == VerdictStatus::Satisfied {
        if let Some(reservation) = &request.completion_reservation {
            compact_state(&mut state, generation);
            let key = operation_key(&identity.session_key, &reservation.operation_id);
            let status = if let Some(existing) = state.operations.get(&key) {
                let exact = matches!(existing.kind, AcceptanceOperationKind::Completion)
                    && existing.session_key == identity.session_key
                    && existing.turn_key == identity.turn_key
                    && existing.contract_digest == request.contract_digest
                    && existing.generation == generation
                    && existing.terminal.is_none()
                    && constant_time_eq(
                        existing.capability_digest.as_bytes(),
                        capability_digest.as_bytes(),
                    );
                if !exact {
                    return Err(HookError::data(
                        "finish-line-operation-exists",
                        "finish-line completion reservation already has a different binding",
                    ));
                }
                "duplicate"
            } else {
                if state.operations.len() >= MAX_OPERATIONS {
                    return Err(HookError::data(
                        "finish-line-state-limit",
                        "finish-line acceptance operation limit is reached",
                    ));
                }
                let sequence = state.next_sequence()?;
                state.operations.insert(
                    key,
                    AcceptanceOperation {
                        session_key: identity.session_key.clone(),
                        turn_key: identity.turn_key.clone(),
                        token_digest: digest_parts(
                            "agent-hook.finish-line.completion-reservation.v1",
                            &[reservation.operation_id.as_bytes()],
                        ),
                        capability_digest: capability_digest.clone(),
                        contract_digest: request.contract_digest.clone(),
                        generation,
                        sequence,
                        binding_digest: digest_parts(
                            "agent-hook.finish-line.completion-binding.v1",
                            &[request.contract_digest.as_bytes()],
                        ),
                        source_operation_key: None,
                        kind: AcceptanceOperationKind::Completion,
                        admission: AdmissionStatus::Admitted,
                        terminal: None,
                    },
                );
                save_state(&store, &state)?;
                "reserved"
            };
            Some(json!({
                "operation_id": reservation.operation_id,
                "status": status,
            }))
        } else {
            None
        }
    } else {
        None
    };
    Ok(Outcome {
        data: json!({
            "schema_version": "agent-hook.finish-line.verdict-result.v1",
            "action": action,
            "aggregate": aggregate.as_str(),
            "generation": generation,
            "contract_digest": request.contract_digest,
            "correlation_id": identity.correlation_id,
            "reason_codes": reason_codes.into_iter().collect::<Vec<_>>(),
            "requirements": requirements,
            "completion_reservation": completion_reservation,
        }),
        text: format!("finish-line acceptance verdict: {}\n", aggregate.as_str()),
        exit_code: if aggregate == VerdictStatus::Satisfied {
            0
        } else {
            1
        },
    })
}

pub(super) fn session_busy(store: &Store, identity: &RequestIdentity) -> Result<bool, HookError> {
    let state = read_state(store, identity)?;
    Ok(state.operations.values().any(|operation| {
        operation.session_key == identity.session_key
            && !matches!(operation.kind, AcceptanceOperationKind::Completion)
            && operation.terminal.is_none()
    }))
}

pub(super) fn repository_completion_reserved(
    store: &Store,
    identity: &RequestIdentity,
) -> Result<bool, HookError> {
    let mut state = read_state(store, identity)?;
    if terminalize_inactive_completion_reservations(store, &mut state) {
        save_state(store, &state)?;
    }
    Ok(completion_reservation_active(store, &state))
}

pub(super) fn release_session(
    store: &Store,
    identity: &RequestIdentity,
    capability_digest: &str,
) -> Result<(), HookError> {
    let mut state = read_state(store, identity)?;
    let mut changed = false;
    for operation in state.operations.values_mut() {
        if operation.session_key == identity.session_key
            && matches!(operation.kind, AcceptanceOperationKind::Completion)
            && operation.terminal.is_none()
            && constant_time_eq(
                operation.capability_digest.as_bytes(),
                capability_digest.as_bytes(),
            )
        {
            operation.terminal = Some(AcceptanceTerminal {
                observation: ObservationStatus::InfrastructureBlocked,
                source_digest: "session-release".to_string(),
                disposition: CompletionDisposition::Applied,
            });
            changed = true;
        }
    }
    if changed {
        save_state(store, &state)?;
    }
    Ok(())
}

pub(super) fn repository_mutation_active(
    store: &Store,
    identity: &RequestIdentity,
) -> Result<bool, HookError> {
    let state = read_state(store, identity)?;
    Ok(state.operations.values().any(|operation| {
        matches!(operation.kind, AcceptanceOperationKind::Mutation) && operation.terminal.is_none()
    }))
}

fn main_shell_active(store: &Store) -> bool {
    store.state.operations.values().any(|operation| {
        operation.kind == StoredOperationKind::Shell && operation.terminal.is_none()
    })
}

fn completion_reservation_active(store: &Store, state: &AcceptanceState) -> bool {
    state.operations.values().any(|operation| {
        matches!(operation.kind, AcceptanceOperationKind::Completion)
            && operation.terminal.is_none()
            && store
                .state
                .sessions
                .get(&operation.session_key)
                .and_then(|session| session.runner_capability_digest.as_deref())
                .is_some_and(|capability| {
                    constant_time_eq(
                        capability.as_bytes(),
                        operation.capability_digest.as_bytes(),
                    )
                })
    })
}

fn terminalize_inactive_completion_reservations(
    store: &Store,
    state: &mut AcceptanceState,
) -> bool {
    let mut changed = false;
    for operation in state.operations.values_mut() {
        if !matches!(operation.kind, AcceptanceOperationKind::Completion)
            || operation.terminal.is_some()
        {
            continue;
        }
        let capability_is_live = store
            .state
            .sessions
            .get(&operation.session_key)
            .and_then(|session| session.runner_capability_digest.as_deref())
            .is_some_and(|capability| {
                constant_time_eq(
                    capability.as_bytes(),
                    operation.capability_digest.as_bytes(),
                )
            });
        if !capability_is_live {
            operation.terminal = Some(AcceptanceTerminal {
                observation: ObservationStatus::InfrastructureBlocked,
                source_digest: "session-orphaned".to_string(),
                disposition: CompletionDisposition::Applied,
            });
            changed = true;
        }
    }
    changed
}

pub(super) fn record_contained_infrastructure_failure(
    store: &Store,
    identity: &RequestIdentity,
    generation: u64,
    target_digest: &str,
    validation_contract_digest: &str,
    source_operation_key: &str,
) -> Result<(), HookError> {
    let current_capability = store
        .state
        .sessions
        .get(&identity.session_key)
        .and_then(|session| session.runner_capability_digest.as_deref())
        .ok_or_else(|| {
            finish_line_unavailable(
                "finish-line-runner-capability-unavailable",
                "finish-line runner capability state is unavailable",
            )
        })?;
    let mut state = read_state(store, identity)?;
    let mut matching = Vec::new();
    for (key, operation) in &state.operations {
        if operation.session_key != identity.session_key
            || operation.generation != generation
            || operation.admission != AdmissionStatus::Admitted
            || operation.terminal.is_some()
            || !constant_time_eq(
                operation.capability_digest.as_bytes(),
                current_capability.as_bytes(),
            )
        {
            continue;
        }
        let Some(ValidatorExecution::ContainedBash {
            target_digest: expected_target,
            validation_contract_digest: expected_contract,
            ..
        }) = operation_execution(&state, identity, operation)?
        else {
            continue;
        };
        if operation.source_operation_key.as_deref() == Some(source_operation_key)
            && expected_target == target_digest
            && expected_contract == validation_contract_digest
        {
            matching.push(key.clone());
        }
    }

    for key in matching {
        let operation = state
            .operations
            .get(&key)
            .cloned()
            .expect("matched acceptance operation");
        let AcceptanceOperationKind::Validator { requirement, .. } = &operation.kind else {
            continue;
        };
        let disposition = if generation != store.state.generation {
            CompletionDisposition::Stale
        } else {
            let evidence = state
                .sessions
                .get_mut(&identity.session_key)
                .and_then(|session| session.evidence.get_mut(requirement));
            if evidence.as_ref().is_none_or(|evidence| {
                evidence.generation != operation.generation
                    || evidence.contract_digest != operation.contract_digest
                    || evidence.attempt_sequence != operation.sequence
            }) {
                CompletionDisposition::Superseded
            } else {
                evidence.expect("acceptance evidence checked").status =
                    RequirementStatus::InfrastructureBlocked;
                CompletionDisposition::Applied
            }
        };
        state
            .operations
            .get_mut(&key)
            .expect("matched acceptance operation")
            .terminal = Some(AcceptanceTerminal {
            observation: ObservationStatus::InfrastructureBlocked,
            source_digest: source_operation_key.to_string(),
            disposition,
        });
    }
    save_state(store, &state)
}

fn normalize_contract(
    requirements: Vec<RequirementRegistration>,
    invalidators: Vec<DefinitionRegistration>,
    snapshot: &ContractSnapshot,
) -> Result<AcceptanceContract, HookError> {
    if requirements.is_empty() || requirements.len() > MAX_REQUIREMENTS {
        return Err(HookError::data(
            "finish-line-acceptance-contract-invalid",
            "finish-line acceptance requires 1..128 named requirements",
        ));
    }
    if invalidators.len() > MAX_INVALIDATORS {
        return Err(HookError::data(
            "finish-line-acceptance-contract-invalid",
            "finish-line acceptance invalidator limit is exceeded",
        ));
    }
    let mut normalized_requirements = BTreeMap::new();
    for requirement in requirements {
        validate_identifier(&requirement.name)?;
        if requirement.validators.is_empty()
            || requirement.validators.len() > MAX_VALIDATORS_PER_REQUIREMENT
        {
            return Err(HookError::data(
                "finish-line-acceptance-contract-invalid",
                "each finish-line acceptance requirement needs 1..16 validators",
            ));
        }
        let mut validators = BTreeMap::new();
        for validator in requirement.validators {
            validate_identifier(&validator.id)?;
            let binding_digest =
                definition_binding_digest(&validator.tool_name, &validator.definition_digest)?;
            let execution = match validator.execution {
                ValidatorExecutionRegistration::HostObserved => ValidatorExecution::HostObserved,
                ValidatorExecutionRegistration::ContainedBash { intent, command } => {
                    validate_intent(&intent)?;
                    validate_command(&command)?;
                    let target = snapshot
                        .targets
                        .iter()
                        .find(|target| target.intent == intent && target.command == command)
                        .ok_or_else(|| {
                            HookError::data(
                                "finish-line-acceptance-contained-validator-unregistered",
                                "contained Bash acceptance validators must match an exact current agent-docs target",
                            )
                        })?;
                    ValidatorExecution::ContainedBash {
                        intent,
                        command_digest: digest_parts(
                            "agent-hook.finish-line.acceptance-command.v1",
                            &[command.as_bytes()],
                        ),
                        target_digest: target.target_digest.clone(),
                        validation_contract_digest: target.contract_digest.clone(),
                    }
                }
            };
            if validators
                .insert(
                    validator.id,
                    ValidatorContract {
                        binding_digest,
                        execution,
                    },
                )
                .is_some()
            {
                return Err(HookError::data(
                    "finish-line-acceptance-contract-invalid",
                    "finish-line acceptance validator ids must be unique per requirement",
                ));
            }
        }
        if normalized_requirements
            .insert(requirement.name, RequirementContract { validators })
            .is_some()
        {
            return Err(HookError::data(
                "finish-line-acceptance-contract-invalid",
                "finish-line acceptance requirement names must be unique",
            ));
        }
    }

    let mut normalized_invalidators = BTreeSet::new();
    for invalidator in invalidators {
        let digest =
            definition_binding_digest(&invalidator.tool_name, &invalidator.definition_digest)?;
        if !normalized_invalidators.insert(digest) {
            return Err(HookError::data(
                "finish-line-acceptance-contract-invalid",
                "finish-line acceptance invalidator definitions must be unique",
            ));
        }
    }
    Ok(AcceptanceContract {
        requirements: normalized_requirements,
        invalidators: normalized_invalidators,
    })
}

fn normalize_admission(
    operation: &AdmitOperation,
) -> Result<(AcceptanceOperationKind, String), HookError> {
    match operation {
        AdmitOperation::Mutation {
            tool_name,
            definition_digest,
        } => Ok((
            AcceptanceOperationKind::Mutation,
            definition_binding_digest(tool_name, definition_digest)?,
        )),
        AdmitOperation::Validator {
            requirement,
            validator_id,
            tool_name,
            definition_digest,
            source_operation_id: _,
        } => {
            validate_identifier(requirement)?;
            validate_identifier(validator_id)?;
            Ok((
                AcceptanceOperationKind::Validator {
                    requirement: requirement.clone(),
                    validator_id: validator_id.clone(),
                },
                definition_binding_digest(tool_name, definition_digest)?,
            ))
        }
    }
}

fn validate_admission_binding(
    session: &AcceptanceSession,
    kind: &AcceptanceOperationKind,
    binding_digest: &str,
    request: &AdmitOperation,
    identity: &RequestIdentity,
) -> Result<Option<String>, HookError> {
    match (kind, request) {
        (AcceptanceOperationKind::Mutation, AdmitOperation::Mutation { .. }) => {
            if session.contract.invalidators.contains(binding_digest) {
                return Ok(None);
            }
            Err(HookError::data(
                "finish-line-acceptance-definition-unregistered",
                "finish-line acceptance operation does not match an exact registered definition",
            ))
        }
        (
            AcceptanceOperationKind::Validator {
                requirement,
                validator_id,
            },
            AdmitOperation::Validator {
                source_operation_id,
                ..
            },
        ) => {
            let validator = session
                .contract
                .requirements
                .get(requirement)
                .and_then(|requirement| requirement.validators.get(validator_id));
            let Some(validator) =
                validator.filter(|validator| validator.binding_digest == binding_digest)
            else {
                return Err(HookError::data(
                    "finish-line-acceptance-definition-unregistered",
                    "finish-line acceptance operation does not match an exact registered definition",
                ));
            };
            match (&validator.execution, source_operation_id.as_deref()) {
                (ValidatorExecution::HostObserved, None) => Ok(None),
                (ValidatorExecution::ContainedBash { .. }, Some(operation_id)) => {
                    validate_identifier(operation_id)?;
                    Ok(Some(operation_key(&identity.session_key, operation_id)))
                }
                (ValidatorExecution::HostObserved, Some(_)) => Err(HookError::data(
                    "finish-line-acceptance-source-operation-invalid",
                    "host-observed validators cannot reserve a contained source operation",
                )),
                (ValidatorExecution::ContainedBash { .. }, None) => Err(HookError::data(
                    "finish-line-acceptance-source-operation-invalid",
                    "contained Bash validators require a planned finish-line run operation",
                )),
            }
        }
        _ => Err(HookError::data(
            "finish-line-acceptance-state-invalid",
            "finish-line acceptance request kind is inconsistent",
        )),
    }
}

fn acceptance_session<'a>(
    state: &'a AcceptanceState,
    identity: &RequestIdentity,
    contract_digest: &str,
) -> Result<&'a AcceptanceSession, HookError> {
    let session = state.sessions.get(&identity.session_key).ok_or_else(|| {
        HookError::data(
            "finish-line-acceptance-contract-missing",
            "finish-line acceptance contract is not registered for this DSH session",
        )
    })?;
    if session.contract_digest != contract_digest {
        return Err(HookError::data(
            "finish-line-acceptance-contract-drift",
            "finish-line acceptance request does not match the registered contract",
        ));
    }
    Ok(session)
}

fn reconcile_reserved_mutation(
    store: &mut Store,
    state: &mut AcceptanceState,
    operation_key: &str,
    operation: &AcceptanceOperation,
) -> Result<(), HookError> {
    if !matches!(operation.kind, AcceptanceOperationKind::Mutation) {
        return Err(HookError::data(
            "finish-line-acceptance-state-invalid",
            "only a finish-line acceptance mutation may have a reserved admission",
        ));
    }
    if store.state.generation.checked_add(1) == Some(operation.generation) {
        store.state.generation = operation.generation;
        compact_obsolete_sessions(&mut store.state);
        store.save()?;
    } else if store.state.generation != operation.generation {
        return Err(finish_line_unavailable(
            "finish-line-acceptance-admission-uncertain",
            "finish-line acceptance mutation reservation cannot be reconciled safely",
        ));
    }
    state
        .operations
        .get_mut(operation_key)
        .expect("reserved acceptance operation")
        .admission = AdmissionStatus::Admitted;
    save_state(store, state)
}

fn admit_outcome(
    identity: &RequestIdentity,
    request: &AdmitRequest,
    kind: &str,
    generation: u64,
) -> Outcome {
    success_outcome(
        json!({
            "schema_version": "agent-hook.finish-line.admit-result.v1",
            "status": "admitted",
            "operation_id": request.operation_id,
            "operation_kind": kind,
            "generation": generation,
            "contract_digest": request.contract_digest,
            "correlation_id": identity.correlation_id,
        }),
        "finish-line acceptance operation admitted\n",
    )
}

fn operation_execution(
    state: &AcceptanceState,
    identity: &RequestIdentity,
    operation: &AcceptanceOperation,
) -> Result<Option<ValidatorExecution>, HookError> {
    let AcceptanceOperationKind::Validator {
        requirement,
        validator_id,
    } = &operation.kind
    else {
        return Ok(None);
    };
    state
        .sessions
        .get(&identity.session_key)
        .filter(|session| session.contract_digest == operation.contract_digest)
        .and_then(|session| session.contract.requirements.get(requirement))
        .and_then(|requirement| requirement.validators.get(validator_id))
        .map(|validator| Some(validator.execution.clone()))
        .ok_or_else(|| {
            HookError::data(
                "finish-line-acceptance-state-invalid",
                "finish-line acceptance validator binding is missing from durable state",
            )
        })
}

fn observation_binding(
    identity: &RequestIdentity,
    observation: &Observation,
    execution: Option<&ValidatorExecution>,
) -> Result<(String, Option<ObservationStatus>, Option<String>), HookError> {
    match (execution, observation.kind) {
        (None | Some(ValidatorExecution::HostObserved), ObservationKind::HostObserved) => {
            let status = observation.status.ok_or_else(|| {
                HookError::data(
                    "finish-line-acceptance-observation-invalid",
                    "host-observed acceptance terminals require one normalized status",
                )
            })?;
            if observation.operation_id.is_some() {
                return Err(HookError::data(
                    "finish-line-acceptance-observation-invalid",
                    "host-observed acceptance terminals cannot cite a contained operation",
                ));
            }
            Ok(("host-observed".to_string(), Some(status), None))
        }
        (Some(ValidatorExecution::ContainedBash { .. }), ObservationKind::ContainedBash) => {
            if observation.status.is_some() {
                return Err(HookError::data(
                    "finish-line-acceptance-observation-invalid",
                    "contained Bash acceptance terminals are derived by nils and reject caller status",
                ));
            }
            let operation_id = observation.operation_id.as_deref().ok_or_else(|| {
                HookError::data(
                    "finish-line-acceptance-observation-invalid",
                    "contained Bash acceptance terminals require the exact finish-line run operation",
                )
            })?;
            validate_identifier(operation_id)?;
            let source_digest = operation_key(&identity.session_key, operation_id);
            Ok((source_digest.clone(), None, Some(source_digest)))
        }
        _ => Err(HookError::data(
            "finish-line-acceptance-observation-source-invalid",
            "finish-line acceptance terminal source does not match the registered validator execution kind",
        )),
    }
}

fn derive_contained_observation(
    store: &Store,
    acceptance_operation: &AcceptanceOperation,
    execution: &ValidatorExecution,
    operation_key: &str,
) -> Result<ObservationStatus, HookError> {
    let ValidatorExecution::ContainedBash {
        target_digest,
        validation_contract_digest,
        ..
    } = execution
    else {
        return Err(HookError::data(
            "finish-line-acceptance-state-invalid",
            "finish-line acceptance contained source has a host-observed contract",
        ));
    };
    let operation = store.state.operations.get(operation_key).ok_or_else(|| {
        HookError::data(
            "finish-line-acceptance-contained-operation-missing",
            "finish-line acceptance contained source is not an authoritative run operation",
        )
    })?;
    let exact_binding = operation.session_key == acceptance_operation.session_key
        && operation.kind == StoredOperationKind::Validation
        && operation.generation == acceptance_operation.generation
        && operation.target_digest.as_deref() == Some(target_digest)
        && operation.contract_digest.as_deref() == Some(validation_contract_digest);
    if !exact_binding {
        return Err(HookError::data(
            "finish-line-acceptance-contained-operation-mismatch",
            "finish-line acceptance contained source does not match the exact validator binding",
        ));
    }
    let terminal = operation.terminal.as_ref().ok_or_else(|| {
        finish_line_temporary(
            "finish-line-acceptance-contained-operation-pending",
            "finish-line acceptance contained source is still pending",
        )
    })?;
    if terminal.disposition != CompletionDisposition::Applied {
        return Err(HookError::data(
            "finish-line-acceptance-contained-operation-stale",
            "finish-line acceptance contained source was not applied to its exact generation",
        ));
    }
    let facts = terminal.execution.as_ref().ok_or_else(|| {
        HookError::data(
            "finish-line-acceptance-state-invalid",
            "finish-line acceptance contained source has no nils-derived execution facts",
        )
    })?;
    if facts.timed_out {
        return Ok(ObservationStatus::TimedOut);
    }
    if facts.signal.is_some() {
        return Ok(ObservationStatus::Signalled);
    }
    if facts.aborted {
        return Ok(ObservationStatus::Cancelled);
    }
    Ok(match terminal.outcome {
        NormalizedOutcome::Success { .. } => ObservationStatus::Succeeded,
        NormalizedOutcome::Failure { .. } => ObservationStatus::Failed,
    })
}

fn observe_outcome(
    identity: &RequestIdentity,
    request: &ObserveRequest,
    status: &str,
    generation: u64,
    observation: ObservationStatus,
) -> Outcome {
    success_outcome(
        json!({
            "schema_version": "agent-hook.finish-line.observe-result.v1",
            "status": status,
            "operation_id": request.operation_id,
            "generation": generation,
            "observation": observation.as_str(),
            "correlation_id": identity.correlation_id,
        }),
        "finish-line acceptance observation recorded\n",
    )
}

fn operation_kind_name(kind: &AcceptanceOperationKind) -> &'static str {
    match kind {
        AcceptanceOperationKind::Mutation => "mutation",
        AcceptanceOperationKind::Validator { .. } => "validator",
        AcceptanceOperationKind::Completion => "completion",
    }
}

fn requirement_verdict(
    state: &AcceptanceState,
    session: &AcceptanceSession,
    session_key: &str,
    requirement: &str,
    generation: u64,
    capability_digest: &str,
    snapshot: &ContractSnapshot,
) -> VerdictStatus {
    let Some(evidence) = session.evidence.get(requirement) else {
        return VerdictStatus::Missing;
    };
    if evidence.generation != generation || evidence.contract_digest != session.contract_digest {
        return VerdictStatus::Missing;
    }
    let validator = session
        .contract
        .requirements
        .get(requirement)
        .and_then(|requirement| requirement.validators.get(&evidence.validator_id));
    let Some(validator) = validator else {
        return VerdictStatus::InfrastructureBlocked;
    };
    if let ValidatorExecution::ContainedBash {
        target_digest,
        validation_contract_digest,
        ..
    } = &validator.execution
        && !snapshot.targets.iter().any(|target| {
            target.target_digest == *target_digest
                && target.contract_digest == *validation_contract_digest
        })
    {
        return VerdictStatus::InfrastructureBlocked;
    }
    match evidence.status {
        RequirementStatus::Satisfied => VerdictStatus::Satisfied,
        RequirementStatus::Failed => VerdictStatus::Failed,
        RequirementStatus::Uncertain => VerdictStatus::Uncertain,
        RequirementStatus::InfrastructureBlocked => VerdictStatus::InfrastructureBlocked,
        RequirementStatus::Active => {
            let operation = state.operations.values().find(|operation| {
                operation.session_key == session_key
                    && operation.sequence == evidence.attempt_sequence
                    && matches!(operation.kind, AcceptanceOperationKind::Validator { .. })
            });
            match operation {
                Some(operation)
                    if operation.admission == AdmissionStatus::Admitted
                        && operation.terminal.is_none()
                        && constant_time_eq(
                            operation.capability_digest.as_bytes(),
                            capability_digest.as_bytes(),
                        ) =>
                {
                    VerdictStatus::Active
                }
                _ => VerdictStatus::InfrastructureBlocked,
            }
        }
    }
}

fn mutation_verdict(
    operation: &AcceptanceOperation,
    session: &AcceptanceSession,
    generation: u64,
    owner_capability_digest: Option<&str>,
) -> VerdictStatus {
    let Some(terminal) = operation.terminal.as_ref() else {
        if operation.admission == AdmissionStatus::Admitted
            && operation.generation == generation
            && owner_capability_digest.is_some_and(|expected| {
                constant_time_eq(operation.capability_digest.as_bytes(), expected.as_bytes())
            })
        {
            return VerdictStatus::Active;
        }
        return VerdictStatus::InfrastructureBlocked;
    };
    if terminal.disposition != CompletionDisposition::Applied {
        return VerdictStatus::Satisfied;
    }
    let fully_revalidated = session.contract.requirements.keys().all(|requirement| {
        session.evidence.get(requirement).is_some_and(|evidence| {
            evidence.generation == generation
                && evidence.contract_digest == session.contract_digest
                && evidence.attempt_sequence > operation.sequence
                && evidence.status == RequirementStatus::Satisfied
        })
    });
    if fully_revalidated {
        return VerdictStatus::Satisfied;
    }
    match terminal.observation {
        ObservationStatus::Cancelled
        | ObservationStatus::TimedOut
        | ObservationStatus::Signalled
        | ObservationStatus::Uncertain => VerdictStatus::Uncertain,
        ObservationStatus::InfrastructureBlocked => VerdictStatus::InfrastructureBlocked,
        ObservationStatus::Succeeded | ObservationStatus::Failed => VerdictStatus::Satisfied,
    }
}

fn mutation_relevant(operation: &AcceptanceOperation, generation: u64) -> bool {
    operation.terminal.is_none() || operation.generation == generation
}

fn acceptance_contract_digest(contract: &AcceptanceContract) -> Result<String, HookError> {
    let bytes = serde_json::to_vec(contract).map_err(|_| {
        HookError::runtime(
            "finish-line-acceptance-contract-serialize-failed",
            "finish-line acceptance contract could not be canonicalized",
        )
    })?;
    Ok(digest_parts(
        "agent-hook.finish-line.acceptance-contract.v1",
        &[&bytes],
    ))
}

fn definition_binding_digest(
    tool_name: &str,
    definition_digest: &str,
) -> Result<String, HookError> {
    validate_identifier(tool_name)?;
    validate_digest(definition_digest)?;
    Ok(digest_parts(
        "agent-hook.finish-line.acceptance-definition.v1",
        &[tool_name.as_bytes(), definition_digest.as_bytes()],
    ))
}

fn validate_digest(value: &str) -> Result<(), HookError> {
    let valid = value.strip_prefix("sha256:").is_some_and(|hex| {
        hex.len() == 64
            && hex
                .bytes()
                .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    });
    if !valid {
        return Err(HookError::data(
            "finish-line-acceptance-digest-invalid",
            "finish-line acceptance digests must be canonical lowercase SHA-256 values",
        ));
    }
    Ok(())
}

fn state_path(store: &Store) -> PathBuf {
    store.state_path.with_extension("acceptance.json")
}

fn read_state(store: &Store, identity: &RequestIdentity) -> Result<AcceptanceState, HookError> {
    let path = state_path(store);
    let file = match OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC | libc::O_NONBLOCK)
        .open(path)
    {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Ok(AcceptanceState::new(&identity.repo_digest));
        }
        Err(_) => {
            return Err(HookError::data(
                "finish-line-acceptance-state-untrusted",
                "finish-line acceptance state could not be opened without following links",
            ));
        }
    };
    verify_private_regular(&file, "finish-line-acceptance-state-untrusted")?;
    let mut bytes = Vec::new();
    file.take(STATE_MAX_BYTES + 1)
        .read_to_end(&mut bytes)
        .map_err(|_| {
            finish_line_unavailable(
                "finish-line-acceptance-state-unavailable",
                "finish-line acceptance state could not be read",
            )
        })?;
    if bytes.len() as u64 > STATE_MAX_BYTES {
        return Err(HookError::data(
            "finish-line-acceptance-state-invalid",
            "finish-line acceptance state exceeds 384 KiB",
        ));
    }
    let state: AcceptanceState = serde_json::from_slice(&bytes).map_err(|_| {
        HookError::data(
            "finish-line-acceptance-state-invalid",
            "finish-line acceptance state does not match its strict schema",
        )
    })?;
    if state.schema_version != STATE_SCHEMA || state.repo_digest != identity.repo_digest {
        return Err(HookError::data(
            "finish-line-acceptance-state-invalid",
            "finish-line acceptance state schema or repository binding is invalid",
        ));
    }
    Ok(state)
}

fn save_state(store: &Store, state: &AcceptanceState) -> Result<(), HookError> {
    let bytes = serde_json::to_vec(state).map_err(|_| {
        HookError::runtime(
            "finish-line-acceptance-state-serialize-failed",
            "finish-line acceptance state could not be serialized",
        )
    })?;
    if bytes.len() as u64 > STATE_MAX_BYTES {
        return Err(HookError::data(
            "finish-line-state-limit",
            "finish-line acceptance state exceeds 384 KiB",
        ));
    }
    write_state_atomic(&state_path(store), &bytes)
}

fn compact_state(state: &mut AcceptanceState, current_generation: u64) {
    if state.operations.len() < COMPACTION_TRIGGER_OPERATIONS {
        return;
    }
    let remove_count = state
        .operations
        .len()
        .saturating_sub(COMPACTED_OPERATION_COUNT);
    let mut terminal = state
        .operations
        .iter()
        .filter_map(|(key, operation)| {
            let compactable = operation.terminal.is_some()
                && (!matches!(operation.kind, AcceptanceOperationKind::Mutation)
                    || operation.generation < current_generation);
            compactable.then(|| (operation.sequence, key.clone()))
        })
        .collect::<Vec<_>>();
    terminal.sort_by(|left, right| left.0.cmp(&right.0).then_with(|| left.1.cmp(&right.1)));
    for (_, key) in terminal.into_iter().take(remove_count) {
        state.operations.remove(&key);
    }
}

fn compact_released_sessions(
    state: &mut AcceptanceState,
    active_sessions: &BTreeMap<String, super::SessionState>,
) {
    if state.sessions.len() < super::MAX_SESSIONS {
        return;
    }
    let remove_count = state.sessions.len().saturating_sub(super::MAX_SESSIONS - 1);
    let mut released = state
        .sessions
        .iter()
        .filter(|(session_key, _)| {
            !active_sessions.contains_key(*session_key)
                && !state.operations.values().any(|operation| {
                    operation.session_key == **session_key && operation.terminal.is_none()
                })
        })
        .map(|(session_key, session)| (session.sequence, session_key.clone()))
        .collect::<Vec<_>>();
    released.sort_by(|left, right| left.0.cmp(&right.0).then_with(|| left.1.cmp(&right.1)));
    for (_, session_key) in released.into_iter().take(remove_count) {
        state.sessions.remove(&session_key);
        state
            .operations
            .retain(|_, operation| operation.session_key != session_key);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn operation(sequence: u64, terminal: bool) -> AcceptanceOperation {
        AcceptanceOperation {
            session_key: "session".to_string(),
            turn_key: "turn".to_string(),
            token_digest: "token".to_string(),
            capability_digest: "capability".to_string(),
            contract_digest: "contract".to_string(),
            generation: 1,
            sequence,
            binding_digest: "binding".to_string(),
            source_operation_key: None,
            kind: AcceptanceOperationKind::Validator {
                requirement: "requirement".to_string(),
                validator_id: "validator".to_string(),
            },
            admission: AdmissionStatus::Admitted,
            terminal: terminal.then_some(AcceptanceTerminal {
                observation: ObservationStatus::Succeeded,
                source_digest: "host-observed".to_string(),
                disposition: CompletionDisposition::Applied,
            }),
        }
    }

    fn session(sequence: u64) -> AcceptanceSession {
        AcceptanceSession {
            contract_digest: "contract".to_string(),
            contract: AcceptanceContract {
                requirements: BTreeMap::new(),
                invalidators: BTreeSet::new(),
            },
            evidence: BTreeMap::new(),
            claimed_sources: BTreeSet::new(),
            sequence,
        }
    }

    #[test]
    fn compaction_keeps_active_operations_and_a_bounded_terminal_window() {
        let mut state = AcceptanceState::new("repo");
        for sequence in 1..=COMPACTION_TRIGGER_OPERATIONS as u64 {
            state
                .operations
                .insert(format!("terminal-{sequence:03}"), operation(sequence, true));
        }
        state
            .operations
            .insert("active".to_string(), operation(300, false));

        compact_state(&mut state, 2);

        assert_eq!(state.operations.len(), COMPACTED_OPERATION_COUNT);
        assert!(state.operations.contains_key("active"));
        assert!(!state.operations.contains_key("terminal-001"));
        assert!(state.operations.contains_key("terminal-256"));
    }

    #[test]
    fn session_pressure_evicts_only_the_oldest_released_acceptance_session() {
        let mut state = AcceptanceState::new("repo");
        for sequence in 0..super::super::MAX_SESSIONS as u64 {
            let key = format!("session-{sequence:03}");
            state.sessions.insert(key.clone(), session(sequence));
            let mut session_operation = operation(sequence, true);
            session_operation.session_key.clone_from(&key);
            state.operations.insert(key.clone(), session_operation);
        }
        let mut active = BTreeMap::new();
        active.insert(
            "session-000".to_string(),
            super::super::SessionState::default(),
        );

        compact_released_sessions(&mut state, &active);

        assert_eq!(state.sessions.len(), super::super::MAX_SESSIONS - 1);
        assert!(state.sessions.contains_key("session-000"));
        assert!(!state.sessions.contains_key("session-001"));
        assert!(!state.operations.contains_key("session-001"));
    }

    #[test]
    fn session_pressure_never_evicts_an_unresolved_older_incarnation() {
        let mut state = AcceptanceState::new("repo");
        for sequence in 0..super::super::MAX_SESSIONS as u64 {
            let key = format!("session-{sequence:03}");
            state.sessions.insert(key.clone(), session(sequence));
            let mut session_operation = operation(sequence, true);
            session_operation.session_key.clone_from(&key);
            state.operations.insert(key.clone(), session_operation);
        }
        state
            .operations
            .get_mut("session-000")
            .expect("oldest operation")
            .terminal = None;

        compact_released_sessions(&mut state, &BTreeMap::new());

        assert_eq!(state.sessions.len(), super::super::MAX_SESSIONS - 1);
        assert!(state.sessions.contains_key("session-000"));
        assert!(state.operations.contains_key("session-000"));
        assert!(!state.sessions.contains_key("session-001"));
    }

    #[test]
    fn a_crash_window_reservation_for_the_next_generation_remains_verdict_relevant() {
        let mut reserved = operation(1, false);
        reserved.kind = AcceptanceOperationKind::Mutation;
        reserved.admission = AdmissionStatus::Reserved;
        reserved.generation = 8;

        assert!(mutation_relevant(&reserved, 7));
        assert_eq!(
            mutation_verdict(&reserved, &session(1), 7, Some("capability")),
            VerdictStatus::InfrastructureBlocked
        );
    }

    #[test]
    fn a_live_mutation_is_active_only_at_its_exact_generation() {
        let mut admitted = operation(1, false);
        admitted.kind = AcceptanceOperationKind::Mutation;
        admitted.generation = 8;

        assert_eq!(
            mutation_verdict(&admitted, &session(1), 8, Some("capability")),
            VerdictStatus::Active
        );
        assert_eq!(
            mutation_verdict(&admitted, &session(1), 9, Some("capability")),
            VerdictStatus::InfrastructureBlocked
        );
    }

    #[test]
    fn contract_canonicalization_is_order_independent_and_redacts_raw_bindings() {
        fn requirement(
            name: &str,
            id: &str,
            tool_name: &str,
            digest: &str,
        ) -> RequirementRegistration {
            RequirementRegistration {
                name: name.to_string(),
                validators: vec![ValidatorRegistration {
                    id: id.to_string(),
                    tool_name: tool_name.to_string(),
                    definition_digest: digest.to_string(),
                    execution: ValidatorExecutionRegistration::HostObserved,
                }],
            }
        }
        fn invalidator(tool_name: &str, digest: &str) -> DefinitionRegistration {
            DefinitionRegistration {
                tool_name: tool_name.to_string(),
                definition_digest: digest.to_string(),
            }
        }
        let snapshot = ContractSnapshot {
            global_digest: "contract".to_string(),
            targets: Vec::new(),
            prior_markers: Vec::new(),
        };
        let first = normalize_contract(
            vec![
                requirement(
                    "beta",
                    "validator-beta",
                    "private_tool_beta",
                    "sha256:bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb",
                ),
                requirement(
                    "alpha",
                    "validator-alpha",
                    "private_tool_alpha",
                    "sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
                ),
            ],
            vec![
                invalidator(
                    "mutation_beta",
                    "sha256:dddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddd",
                ),
                invalidator(
                    "mutation_alpha",
                    "sha256:cccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccc",
                ),
            ],
            &snapshot,
        )
        .expect("first contract");
        let second = normalize_contract(
            vec![
                requirement(
                    "alpha",
                    "validator-alpha",
                    "private_tool_alpha",
                    "sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
                ),
                requirement(
                    "beta",
                    "validator-beta",
                    "private_tool_beta",
                    "sha256:bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb",
                ),
            ],
            vec![
                invalidator(
                    "mutation_alpha",
                    "sha256:cccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccc",
                ),
                invalidator(
                    "mutation_beta",
                    "sha256:dddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddd",
                ),
            ],
            &snapshot,
        )
        .expect("second contract");

        assert_eq!(
            acceptance_contract_digest(&first).expect("first digest"),
            acceptance_contract_digest(&second).expect("second digest")
        );
        let stored = serde_json::to_string(&first).expect("stored contract");
        for raw in [
            "private_tool_alpha",
            "private_tool_beta",
            "mutation_alpha",
            "mutation_beta",
            "sha256:aaaaaaaa",
            "sha256:bbbbbbbb",
            "sha256:cccccccc",
            "sha256:dddddddd",
        ] {
            assert!(!stored.contains(raw), "stored raw provider binding: {raw}");
        }
    }
}

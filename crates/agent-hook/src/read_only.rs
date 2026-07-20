use std::collections::BTreeSet;
use std::ffi::{OsStr, OsString};
use std::fs;
use std::os::unix::fs::{MetadataExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};

use nils_common::execution_effect::{
    CapabilityClass, Effect, OPERATION_EFFECT_VERSION, OperationEffectDescriptor, ProviderEffect,
    argv_digest, cwd_digest, executable_digest,
};
use serde::Deserialize;
use serde_json::Value;

use crate::error::HookError;
use crate::model::NormalizedRequest;

const MAX_COMMAND_BYTES: usize = 32 * 1024;
const MAX_ARGUMENTS: usize = 256;
const MAX_ARGUMENT_BYTES: usize = 8 * 1024;
const MAX_DESCRIPTOR_AGE_SECONDS: i64 = 5;

#[derive(Debug)]
pub(crate) struct Candidate {
    executable: PathBuf,
    tool: String,
    argv: Vec<OsString>,
    cwd: PathBuf,
}

impl Candidate {
    pub(crate) fn descriptor_command(&self) -> Command {
        let mut command = Command::new(&self.executable);
        command
            .arg("operation-effect")
            .args(["--format", "json", "--"])
            .args(&self.argv)
            .current_dir(&self.cwd)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        command
    }
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct DescriptorEnvelope {
    schema_version: String,
    ok: bool,
    data: Option<OperationEffectDescriptor>,
    #[serde(default)]
    warnings: Vec<String>,
    error: Option<Value>,
}

pub(crate) fn candidate(raw: &[u8], request: &NormalizedRequest) -> Result<Candidate, HookError> {
    if request.event != "PreToolUse" {
        return Err(rejected(
            "read-only-event-unsupported",
            "read-only evidence is defined only for PreToolUse",
        ));
    }
    let value: Value = serde_json::from_slice(raw).map_err(|_| {
        rejected(
            "read-only-command-unavailable",
            "provider payload has no verifiable command",
        )
    })?;
    let object = value.as_object().ok_or_else(|| {
        rejected(
            "read-only-command-unavailable",
            "provider payload has no verifiable command",
        )
    })?;
    let command = crate::adapter::command_text(object).ok_or_else(|| {
        rejected(
            "read-only-command-unavailable",
            "provider payload has no verifiable command",
        )
    })?;
    let current_executable = std::env::current_exe().map_err(|_| {
        rejected(
            "read-only-producer-untrusted",
            "current agent-hook executable is unavailable",
        )
    })?;
    let execution_path = request.execution_path.as_deref().ok_or_else(|| {
        rejected(
            "read-only-binding-mismatch",
            "request execution directory is unavailable for descriptor binding",
        )
    })?;
    candidate_from_command(
        command,
        &current_executable,
        request.binding_roots.as_slice(),
        execution_path,
    )
}

fn candidate_from_command(
    command: &str,
    current_executable: &Path,
    binding_roots: &[PathBuf],
    execution_path: &Path,
) -> Result<Candidate, HookError> {
    let mut words = parse_simple_command(command)?;
    if words.is_empty() {
        return Err(rejected(
            "read-only-command-unavailable",
            "provider payload has no verifiable command",
        ));
    }
    let program = words.remove(0);
    let program_path = Path::new(&program);
    let tool = program_path
        .file_name()
        .and_then(OsStr::to_str)
        .filter(|name| matches!(*name, "agent-docs" | "forge-cli"))
        .ok_or_else(|| {
            rejected(
                "read-only-command-unsupported",
                "command has no trusted operation-effect producer",
            )
        })?
        .to_string();
    let executable = resolve_executable(program_path).ok_or_else(|| {
        rejected(
            "read-only-producer-unavailable",
            "operation-effect producer is unavailable",
        )
    })?;
    let current_executable = fs::canonicalize(current_executable).map_err(|_| {
        rejected(
            "read-only-producer-untrusted",
            "current agent-hook executable is unavailable",
        )
    })?;
    let executable = fs::canonicalize(executable).map_err(|_| {
        rejected(
            "read-only-producer-untrusted",
            "operation-effect producer is not canonical",
        )
    })?;
    let metadata = fs::metadata(&executable).map_err(|_| {
        rejected(
            "read-only-producer-untrusted",
            "operation-effect producer metadata is unavailable",
        )
    })?;
    let cwd = fs::canonicalize(execution_path).map_err(|_| {
        rejected(
            "read-only-binding-mismatch",
            "request execution directory is unavailable for descriptor binding",
        )
    })?;
    if !cwd.is_dir() {
        return Err(rejected(
            "read-only-binding-mismatch",
            "request execution directory is not a directory",
        ));
    }
    if !metadata.is_file()
        || metadata.uid() != unsafe { libc::geteuid() }
        || metadata.permissions().mode() & 0o111 == 0
        || metadata.permissions().mode() & 0o022 != 0
        || current_executable.parent() != executable.parent()
        || binding_roots
            .iter()
            .any(|root| executable.starts_with(root))
    {
        return Err(rejected(
            "read-only-producer-untrusted",
            "operation-effect producer is outside the same-release trust boundary",
        ));
    }
    Ok(Candidate {
        executable,
        tool,
        argv: words.into_iter().map(OsString::from).collect(),
        cwd,
    })
}

fn resolve_executable(program: &Path) -> Option<PathBuf> {
    if program.components().count() > 1 || program.is_absolute() {
        return program.is_file().then(|| program.to_path_buf());
    }
    std::env::var_os("PATH")
        .into_iter()
        .flat_map(|path| std::env::split_paths(&path).collect::<Vec<_>>())
        .map(|directory| directory.join(program))
        .find(|candidate| candidate.is_file())
}

pub(crate) fn verify_output(candidate: &Candidate, output: &Output) -> Result<(), HookError> {
    verify_output_at(
        candidate,
        output,
        &candidate.cwd,
        jiff::Timestamp::now().as_second(),
    )
}

fn verify_output_at(
    candidate: &Candidate,
    output: &Output,
    cwd: &Path,
    now_epoch: i64,
) -> Result<(), HookError> {
    if !output.status.success() || !output.stderr.is_empty() {
        return Err(rejected(
            "read-only-descriptor-unavailable",
            "operation-effect producer did not return one clean descriptor",
        ));
    }
    let envelope: DescriptorEnvelope = serde_json::from_slice(&output.stdout).map_err(|_| {
        rejected(
            "read-only-descriptor-malformed",
            "operation-effect descriptor envelope is malformed",
        )
    })?;
    let expected_schema = format!("cli.{}.operation-effect.v1", candidate.tool);
    if envelope.schema_version != expected_schema
        || !envelope.ok
        || !envelope.warnings.is_empty()
        || envelope.error.is_some()
    {
        return Err(rejected(
            "read-only-descriptor-malformed",
            "operation-effect descriptor envelope is not an exact success record",
        ));
    }
    let descriptor = envelope.data.ok_or_else(|| {
        rejected(
            "read-only-descriptor-malformed",
            "operation-effect descriptor envelope has no data",
        )
    })?;
    verify_descriptor(candidate, &descriptor, cwd, now_epoch)
}

fn verify_descriptor(
    candidate: &Candidate,
    descriptor: &OperationEffectDescriptor,
    cwd: &Path,
    now_epoch: i64,
) -> Result<(), HookError> {
    if descriptor.schema_version != OPERATION_EFFECT_VERSION
        || descriptor.capability_class != CapabilityClass::ToolContract
    {
        return Err(rejected(
            "read-only-descriptor-unsupported",
            "operation-effect descriptor schema or capability class is unsupported",
        ));
    }
    if descriptor.effect != Effect::ReadOnly
        || descriptor.provider_effect == ProviderEffect::NetworkWrite
    {
        return Err(rejected(
            "read-only-effect-rejected",
            "operation is not described as read-only",
        ));
    }
    if descriptor.producer.tool != candidate.tool
        || descriptor.producer.release != env!("CARGO_PKG_VERSION")
        || descriptor.producer.executable_digest
            != executable_digest(&candidate.executable).map_err(|_| {
                rejected(
                    "read-only-producer-untrusted",
                    "operation-effect producer digest is unavailable",
                )
            })?
    {
        return Err(rejected(
            "read-only-producer-mismatch",
            "operation-effect producer identity does not match agent-hook",
        ));
    }
    if descriptor.binding.argv_digest != argv_digest(&candidate.argv)
        || descriptor.binding.cwd_digest
            != cwd_digest(cwd).map_err(|_| {
                rejected(
                    "read-only-binding-mismatch",
                    "current directory is unavailable for descriptor binding",
                )
            })?
        || !valid_digest(&descriptor.binding.target_digest)
    {
        return Err(rejected(
            "read-only-binding-mismatch",
            "operation-effect descriptor is not bound to this request",
        ));
    }
    if descriptor.issued_at_epoch > now_epoch
        || now_epoch.saturating_sub(descriptor.issued_at_epoch) > MAX_DESCRIPTOR_AGE_SECONDS
    {
        return Err(rejected(
            "read-only-descriptor-stale",
            "operation-effect descriptor is outside its freshness window",
        ));
    }
    if !valid_identifier(&descriptor.operation)
        || descriptor.managed_state_reads.len() > 16
        || descriptor
            .managed_state_reads
            .iter()
            .any(|item| !valid_identifier(item))
        || descriptor
            .managed_state_reads
            .iter()
            .collect::<BTreeSet<_>>()
            .len()
            != descriptor.managed_state_reads.len()
    {
        return Err(rejected(
            "read-only-descriptor-malformed",
            "operation-effect descriptor vocabulary is malformed",
        ));
    }
    Ok(())
}

fn valid_digest(value: &str) -> bool {
    value.strip_prefix("sha256:").is_some_and(|digest| {
        digest.len() == 64
            && digest
                .bytes()
                .all(|byte| byte.is_ascii_digit() || matches!(byte, b'a'..=b'f'))
    })
}

fn valid_identifier(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 96
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'-' | b'_'))
}

fn parse_simple_command(command: &str) -> Result<Vec<String>, HookError> {
    if command.is_empty() || command.len() > MAX_COMMAND_BYTES {
        return Err(rejected(
            "read-only-command-unsupported",
            "command is empty or exceeds the audited command limit",
        ));
    }
    let mut words = Vec::new();
    let mut word = String::new();
    let mut started = false;
    let mut chars = command.chars().peekable();
    let mut quote = None;
    while let Some(character) = chars.next() {
        if character == '\0' || (character.is_control() && !character.is_ascii_whitespace()) {
            return Err(rejected(
                "read-only-command-unsupported",
                "command contains unsupported control bytes",
            ));
        }
        match quote {
            Some('\'') => {
                if character == '\'' {
                    quote = None;
                } else {
                    word.push(character);
                }
                started = true;
            }
            Some('"') => {
                match character {
                    '"' => quote = None,
                    '$' | '`' => {
                        return Err(rejected(
                            "read-only-command-unsupported",
                            "command contains shell expansion",
                        ));
                    }
                    '\\' => {
                        let next = chars.next().ok_or_else(|| {
                            rejected(
                                "read-only-command-unsupported",
                                "command ends with an incomplete escape",
                            )
                        })?;
                        if matches!(next, '"' | '\\') {
                            word.push(next);
                        } else {
                            word.push('\\');
                            word.push(next);
                        }
                    }
                    '\n' | '\r' => {
                        return Err(rejected(
                            "read-only-command-unsupported",
                            "command contains multiple shell statements",
                        ));
                    }
                    _ => word.push(character),
                }
                started = true;
            }
            None => match character {
                character if character.is_ascii_whitespace() => {
                    if character == '\n' || character == '\r' {
                        return Err(rejected(
                            "read-only-command-unsupported",
                            "command contains multiple shell statements",
                        ));
                    }
                    if started {
                        push_word(&mut words, &mut word)?;
                        started = false;
                    }
                }
                '\'' | '"' => {
                    quote = Some(character);
                    started = true;
                }
                '\\' => {
                    let next = chars.next().ok_or_else(|| {
                        rejected(
                            "read-only-command-unsupported",
                            "command ends with an incomplete escape",
                        )
                    })?;
                    if next == '\n' || next == '\r' {
                        return Err(rejected(
                            "read-only-command-unsupported",
                            "command contains multiple shell statements",
                        ));
                    }
                    word.push(next);
                    started = true;
                }
                ';' | '&' | '|' | '<' | '>' | '(' | ')' | '$' | '`' | '*' | '?' | '[' | ']'
                | '{' | '}' | '#' => {
                    return Err(rejected(
                        "read-only-command-unsupported",
                        "command contains shell composition or expansion",
                    ));
                }
                '~' if !started => {
                    return Err(rejected(
                        "read-only-command-unsupported",
                        "command contains shell expansion",
                    ));
                }
                _ => {
                    word.push(character);
                    started = true;
                }
            },
            Some(_) => unreachable!("only single and double quotes are represented"),
        }
        if word.len() > MAX_ARGUMENT_BYTES {
            return Err(rejected(
                "read-only-command-unsupported",
                "command argument exceeds the audited limit",
            ));
        }
    }
    if quote.is_some() {
        return Err(rejected(
            "read-only-command-unsupported",
            "command contains an unterminated quote",
        ));
    }
    if started {
        push_word(&mut words, &mut word)?;
    }
    Ok(words)
}

fn push_word(words: &mut Vec<String>, word: &mut String) -> Result<(), HookError> {
    if words.len() >= MAX_ARGUMENTS {
        return Err(rejected(
            "read-only-command-unsupported",
            "command exceeds the audited argument limit",
        ));
    }
    words.push(std::mem::take(word));
    Ok(())
}

fn rejected(code: &'static str, message: &'static str) -> HookError {
    HookError::data(code, message)
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::os::unix::fs::PermissionsExt;
    use std::os::unix::process::ExitStatusExt;
    use std::process::{ExitStatus, Output};

    use nils_common::execution_effect::{
        CapabilityClass, Effect, InvocationBinding, OPERATION_EFFECT_VERSION,
        OperationEffectDescriptor, ProducerIdentity, ProviderEffect, argv_digest, cwd_digest,
        digest_parts, executable_digest,
    };

    use super::{
        candidate_from_command, parse_simple_command, verify_descriptor, verify_output_at,
    };

    fn fixture() -> (
        tempfile::TempDir,
        super::Candidate,
        OperationEffectDescriptor,
    ) {
        let temp = tempfile::TempDir::new().expect("tempdir");
        let hook = temp.path().join("agent-hook");
        let producer = temp.path().join("agent-docs");
        fs::write(&hook, b"hook").expect("hook file");
        fs::write(&producer, b"producer").expect("producer file");
        fs::set_permissions(&hook, fs::Permissions::from_mode(0o700)).expect("hook mode");
        fs::set_permissions(&producer, fs::Permissions::from_mode(0o700)).expect("producer mode");
        let command = format!("{} preflight --intent project-dev", producer.display());
        let candidate =
            candidate_from_command(&command, &hook, &[], temp.path()).expect("candidate");
        let now = jiff::Timestamp::now().as_second();
        let descriptor = OperationEffectDescriptor {
            schema_version: OPERATION_EFFECT_VERSION.to_string(),
            capability_class: CapabilityClass::ToolContract,
            producer: ProducerIdentity {
                tool: "agent-docs".to_string(),
                release: env!("CARGO_PKG_VERSION").to_string(),
                executable_digest: executable_digest(&producer).expect("executable digest"),
            },
            operation: "preflight".to_string(),
            effect: Effect::ReadOnly,
            provider_effect: ProviderEffect::LocalRead,
            managed_state_reads: vec!["catalog".to_string()],
            binding: InvocationBinding {
                argv_digest: argv_digest(&candidate.argv),
                cwd_digest: cwd_digest(temp.path()).expect("cwd digest"),
                target_digest: digest_parts(std::iter::empty()),
            },
            issued_at_epoch: now,
        };
        (temp, candidate, descriptor)
    }

    #[test]
    fn accepts_an_exact_fresh_same_release_descriptor() {
        let (temp, candidate, descriptor) = fixture();
        verify_descriptor(
            &candidate,
            &descriptor,
            temp.path(),
            descriptor.issued_at_epoch,
        )
        .expect("verified");
    }

    #[test]
    fn rejects_forged_stale_mismatched_and_reserved_evidence() {
        let (temp, candidate, descriptor) = fixture();
        let now = descriptor.issued_at_epoch;
        let mut cases = Vec::new();
        let mut forged = descriptor.clone();
        forged.producer.release = "0.0.0".to_string();
        cases.push((forged, "read-only-producer-mismatch"));
        let mut stale = descriptor.clone();
        stale.issued_at_epoch = now - 6;
        cases.push((stale, "read-only-descriptor-stale"));
        let mut mismatched = descriptor.clone();
        mismatched.binding.argv_digest = digest_parts([b"different".as_slice()]);
        cases.push((mismatched, "read-only-binding-mismatch"));
        let mut reserved = descriptor.clone();
        reserved.capability_class = CapabilityClass::HostAttested;
        cases.push((reserved, "read-only-descriptor-unsupported"));
        let mut mutation = descriptor.clone();
        mutation.effect = Effect::Mutation;
        cases.push((mutation, "read-only-effect-rejected"));

        for (candidate_descriptor, expected) in cases {
            let error = verify_descriptor(&candidate, &candidate_descriptor, temp.path(), now)
                .expect_err(expected);
            assert_eq!(error.code, expected);
        }
    }

    #[test]
    fn strict_envelope_parser_rejects_additive_or_nonclean_evidence() {
        let (temp, candidate, descriptor) = fixture();
        let now = descriptor.issued_at_epoch;
        let exact = serde_json::json!({
            "schema_version": "cli.agent-docs.operation-effect.v1",
            "ok": true,
            "data": descriptor,
            "warnings": []
        });
        let output = Output {
            status: ExitStatus::from_raw(0),
            stdout: serde_json::to_vec(&exact).expect("descriptor JSON"),
            stderr: Vec::new(),
        };
        verify_output_at(&candidate, &output, temp.path(), now).expect("exact envelope");

        let mut additive = exact;
        additive["untrusted"] = serde_json::json!(true);
        let output = Output {
            status: ExitStatus::from_raw(0),
            stdout: serde_json::to_vec(&additive).expect("descriptor JSON"),
            stderr: Vec::new(),
        };
        assert_eq!(
            verify_output_at(&candidate, &output, temp.path(), now)
                .expect_err("unknown field")
                .code,
            "read-only-descriptor-malformed"
        );
    }

    #[test]
    fn parser_accepts_literal_quoting_and_rejects_shell_composition() {
        assert_eq!(
            parse_simple_command("agent-docs preflight --intent 'project-dev'")
                .expect("simple command"),
            ["agent-docs", "preflight", "--intent", "project-dev"]
        );
        for command in [
            "agent-docs preflight; touch changed",
            "agent-docs preflight && echo changed",
            "agent-docs $(touch changed)",
            "agent-docs preflight > result",
            "agent-docs preflight\nforge-cli issue view 670",
        ] {
            assert_eq!(
                parse_simple_command(command).expect_err(command).code,
                "read-only-command-unsupported"
            );
        }
    }

    #[test]
    fn descriptor_command_uses_the_request_bound_cwd() {
        let (temp, candidate, _) = fixture();
        let expected = temp.path().canonicalize().expect("canonical fixture cwd");

        assert_eq!(
            candidate.descriptor_command().get_current_dir(),
            Some(expected.as_path())
        );
    }
}

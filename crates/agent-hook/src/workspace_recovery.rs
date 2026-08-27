//! Strict, read-only workspace recovery facts for a denied DSH lease.

use std::env;
use std::fs;
use std::io::{self, Read};
use std::path::{Path, PathBuf};

use git2::Repository;
use serde::{Deserialize, Serialize, de::DeserializeOwned};
use serde_json::{Value, json};

use crate::dsh_policy::{GitLayout, git_layout};
use crate::error::HookError;
use crate::git_inspection::{DirtyEntry, dirty_entries};

const PROTOCOL_VERSION: u64 = 1;
const INSPECT_SCHEMA: &str = "agent-hook.workspace-recovery.inspect.v1";
const VERIFY_SCHEMA: &str = "agent-hook.workspace-recovery.verify-handoff.v1";
const RESULT_SCHEMA: &str = "agent-hook.workspace-recovery.result.v1";
const REQUEST_MAX_BYTES: u64 = 64 * 1024;
const RESULT_MAX_BYTES: usize = 192 * 1024;
const MAX_WORKTREES: usize = 512;
const MAX_PATH_BYTES: usize = 16 * 1024;
const MAX_BRANCH_BYTES: usize = 512;

#[derive(Clone, Copy, Debug)]
pub(crate) enum Operation {
    Inspect,
    VerifyHandoff,
}

pub(crate) struct Outcome {
    pub(crate) data: Value,
    pub(crate) text: String,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct InspectRequest {
    #[serde(rename = "schema_version")]
    _schema_version: String,
    version: u64,
    cwd: PathBuf,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct VerifyRequest {
    #[serde(rename = "schema_version")]
    _schema_version: String,
    version: u64,
    cwd: PathBuf,
    handoff_path: PathBuf,
}

#[derive(Debug, Serialize)]
struct Checkout {
    path: String,
    branch: Option<String>,
    head: Option<String>,
    managed: bool,
    dirty_entries: Vec<DirtyEntry>,
    dirty_entries_omitted: usize,
}

#[derive(Clone, Debug, Serialize)]
struct Worktree {
    path: String,
    branch: Option<String>,
    head: Option<String>,
    bare: bool,
    detached: bool,
    prunable: bool,
    managed: bool,
}

#[derive(Debug, Serialize)]
struct Handoff {
    status: &'static str,
    path: String,
    branch: String,
    head: String,
}

#[derive(Debug, Serialize)]
struct ResultPayload {
    schema_version: &'static str,
    action: &'static str,
    state: &'static str,
    checkout: Checkout,
    worktrees: Vec<Worktree>,
    worktrees_omitted: usize,
    handoff: Option<Handoff>,
}

fn wire_invalid() -> HookError {
    HookError::data(
        "workspace-recovery-wire-invalid",
        "workspace recovery request does not match the strict protocol",
    )
}

fn unavailable(code: &'static str, message: &'static str) -> HookError {
    HookError::unavailable_with(
        code,
        message,
        json!({
            "retryable": true,
            "next_action": "verify-checkout-and-retry",
            "recovery": {
                "kind": "bounded-retry",
                "max_attempts": 1,
            },
        }),
    )
}

fn read_request<T: DeserializeOwned>(expected_schema: &str) -> Result<T, HookError> {
    let mut bytes = Vec::new();
    io::stdin()
        .take(REQUEST_MAX_BYTES.saturating_add(1))
        .read_to_end(&mut bytes)
        .map_err(|_| wire_invalid())?;
    if bytes.is_empty() || bytes.len() as u64 > REQUEST_MAX_BYTES {
        return Err(wire_invalid());
    }
    let value = crate::strict_json::from_slice(&bytes).map_err(|_| wire_invalid())?;
    if value.get("schema_version").and_then(Value::as_str) != Some(expected_schema) {
        return Err(wire_invalid());
    }
    serde_json::from_value(value).map_err(|_| wire_invalid())
}

fn validate_request(version: u64, cwd: &Path) -> Result<GitLayout, HookError> {
    if version != PROTOCOL_VERSION || !cwd.is_absolute() {
        return Err(wire_invalid());
    }
    git_layout(cwd).ok_or_else(|| {
        unavailable(
            "workspace-recovery-checkout-unavailable",
            "workspace recovery checkout is unavailable",
        )
    })
}

fn display_path(path: &Path) -> Result<String, HookError> {
    let value = path.to_str().ok_or_else(|| {
        HookError::data(
            "workspace-recovery-path-invalid",
            "workspace recovery path is not valid UTF-8",
        )
    })?;
    if value.is_empty() || value.len() > MAX_PATH_BYTES || value.chars().any(char::is_control) {
        return Err(HookError::data(
            "workspace-recovery-path-invalid",
            "workspace recovery path is invalid",
        ));
    }
    Ok(value.to_string())
}

fn display_branch(value: Option<&str>) -> Result<Option<String>, HookError> {
    value
        .map(|value| {
            if value.is_empty()
                || value.len() > MAX_BRANCH_BYTES
                || value.chars().any(char::is_control)
            {
                return Err(HookError::data(
                    "workspace-recovery-branch-invalid",
                    "workspace recovery branch is invalid",
                ));
            }
            Ok(value.to_string())
        })
        .transpose()
}

fn canonical_or_raw(path: &Path) -> PathBuf {
    fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf())
}

fn absolute_or_existing(path: PathBuf) -> Option<PathBuf> {
    if path.exists() {
        fs::canonicalize(path).ok()
    } else if path.is_absolute() {
        Some(path)
    } else {
        env::current_dir().ok().map(|cwd| cwd.join(path))
    }
}

fn agent_home() -> Option<PathBuf> {
    if let Some(value) = env::var_os("AGENT_HOME").filter(|value| !value.is_empty()) {
        return absolute_or_existing(PathBuf::from(value));
    }
    if let Some(value) = env::var_os("XDG_STATE_HOME").filter(|value| !value.is_empty()) {
        return absolute_or_existing(PathBuf::from(value).join("agent-runtime-kit"));
    }
    if let Some(value) = env::var_os("HOME").filter(|value| !value.is_empty()) {
        return absolute_or_existing(PathBuf::from(value).join(".local/state/agent-runtime-kit"));
    }
    Some(env::temp_dir().join("agent-runtime-kit"))
}

fn repo_key(repo_root: &Path) -> String {
    let basename = repo_root
        .file_name()
        .and_then(|value| value.to_str())
        .unwrap_or("repo");
    let mut slug = String::new();
    let mut last_dash = false;
    for character in basename.chars() {
        if character.is_ascii_alphanumeric() {
            slug.push(character.to_ascii_lowercase());
            last_dash = false;
        } else if matches!(character, '-' | '_' | '.') {
            slug.push(character);
            last_dash = false;
        } else if !last_dash {
            slug.push('-');
            last_dash = true;
        }
    }
    let slug = slug.trim_matches(['-', '_', '.']);
    let slug = slug.chars().take(80).collect::<String>();
    let slug = slug.trim_matches(['-', '_', '.']);
    let slug = if slug.is_empty() { "repo" } else { slug };
    let mut hash = 0xcbf2_9ce4_8422_2325_u64;
    for byte in repo_root.to_string_lossy().as_bytes() {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    format!("{slug}-{:08x}", hash as u32)
}

fn primary_root(layout: &GitLayout) -> Result<PathBuf, HookError> {
    let candidate = layout.common_dir.parent().filter(|_| {
        layout
            .common_dir
            .file_name()
            .is_some_and(|name| name == ".git")
    });
    candidate
        .and_then(|path| fs::canonicalize(path).ok())
        .ok_or_else(|| {
            unavailable(
                "workspace-recovery-repository-unavailable",
                "workspace recovery repository root is unavailable",
            )
        })
}

fn repository_facts(path: &Path, managed_root: &Path) -> Result<Worktree, HookError> {
    let canonical = fs::canonicalize(path).map_err(|_| {
        unavailable(
            "workspace-recovery-worktree-unavailable",
            "workspace recovery worktree is unavailable",
        )
    })?;
    let repository = Repository::open(&canonical).map_err(|_| {
        unavailable(
            "workspace-recovery-worktree-unavailable",
            "workspace recovery worktree is unavailable",
        )
    })?;
    let head = repository.head().ok();
    let branch = head
        .as_ref()
        .filter(|value| value.is_branch())
        .and_then(|value| value.shorthand().ok())
        .map(str::to_string);
    let head_id = head
        .as_ref()
        .and_then(|value| value.target())
        .map(|id| id.to_string());
    let detached = repository.head_detached().unwrap_or(true);
    Ok(Worktree {
        path: display_path(&canonical)?,
        branch: display_branch(branch.as_deref())?,
        head: head_id,
        bare: repository.is_bare(),
        detached,
        prunable: false,
        managed: canonical.starts_with(managed_root),
    })
}

fn prunable_worktree(path: &Path, managed_root: &Path) -> Result<Worktree, HookError> {
    let projected = canonical_or_raw(path);
    if !projected.is_absolute() {
        return Err(unavailable(
            "workspace-recovery-worktree-unavailable",
            "workspace recovery worktree is unavailable",
        ));
    }
    Ok(Worktree {
        path: display_path(&projected)?,
        branch: None,
        head: None,
        bare: false,
        detached: true,
        prunable: true,
        managed: projected.starts_with(managed_root),
    })
}

fn inventory(layout: &GitLayout) -> Result<Vec<Worktree>, HookError> {
    let primary = primary_root(layout)?;
    let home = agent_home().ok_or_else(|| {
        unavailable(
            "workspace-recovery-agent-home-unavailable",
            "workspace recovery managed-worktree root is unavailable",
        )
    })?;
    let managed_root = canonical_or_raw(&home.join("worktrees").join(repo_key(&primary)));
    let repository = Repository::open(&primary).map_err(|_| {
        unavailable(
            "workspace-recovery-repository-unavailable",
            "workspace recovery repository is unavailable",
        )
    })?;
    let mut entries = vec![repository_facts(&primary, &managed_root)?];
    let names = repository.worktrees().map_err(|_| {
        unavailable(
            "workspace-recovery-worktrees-unavailable",
            "workspace recovery worktree inventory is unavailable",
        )
    })?;
    if names.len() > MAX_WORKTREES.saturating_sub(1) {
        return Err(HookError::data(
            "workspace-recovery-worktrees-too-large",
            "workspace recovery worktree inventory exceeds its bound",
        ));
    }
    for name in names.iter() {
        let Ok(Some(name)) = name else {
            return Err(unavailable(
                "workspace-recovery-worktrees-unavailable",
                "workspace recovery worktree inventory is unavailable",
            ));
        };
        let worktree = repository.find_worktree(name).map_err(|_| {
            unavailable(
                "workspace-recovery-worktrees-unavailable",
                "workspace recovery worktree inventory is unavailable",
            )
        })?;
        let prunable = worktree.is_prunable(None).map_err(|_| {
            unavailable(
                "workspace-recovery-worktrees-unavailable",
                "workspace recovery worktree inventory is unavailable",
            )
        })?;
        entries.push(if prunable {
            prunable_worktree(worktree.path(), &managed_root)?
        } else {
            repository_facts(worktree.path(), &managed_root)?
        });
    }
    entries.sort_by(|left, right| left.path.cmp(&right.path));
    entries.dedup_by(|left, right| left.path == right.path);
    Ok(entries)
}

fn inspect(
    layout: GitLayout,
    action: &'static str,
    handoff_path: Option<&Path>,
) -> Result<ResultPayload, HookError> {
    let entries = inventory(&layout)?;
    let current_path = display_path(&layout.root)?;
    let current = entries
        .iter()
        .find(|entry| entry.path == current_path)
        .ok_or_else(|| {
            unavailable(
                "workspace-recovery-checkout-unavailable",
                "workspace recovery checkout is unavailable",
            )
        })?;
    let current_dirty = dirty_entries(&layout)?;
    let checkout = Checkout {
        path: current.path.clone(),
        branch: current.branch.clone(),
        head: current.head.clone(),
        managed: current.managed,
        dirty_entries: current_dirty,
        dirty_entries_omitted: 0,
    };
    let handoff = if let Some(requested) = handoff_path {
        if !requested.is_absolute() {
            return Err(wire_invalid());
        }
        let canonical = fs::canonicalize(requested).map_err(|_| {
            HookError::data(
                "workspace-recovery-handoff-invalid",
                "workspace recovery handoff path is invalid",
            )
        })?;
        let projected = display_path(&canonical)?;
        let candidate = entries
            .iter()
            .find(|entry| entry.path == projected)
            .filter(|entry| {
                entry.path != current.path
                    && entry.managed
                    && !entry.bare
                    && !entry.detached
                    && !entry.prunable
                    && entry.branch.is_some()
                    && entry.head.is_some()
            })
            .ok_or_else(|| {
                HookError::data(
                    "workspace-recovery-handoff-ineligible",
                    "workspace recovery handoff is not an eligible managed worktree",
                )
            })?;
        let candidate_layout = git_layout(&canonical).ok_or_else(|| {
            unavailable(
                "workspace-recovery-handoff-unavailable",
                "workspace recovery handoff checkout is unavailable",
            )
        })?;
        if !dirty_entries(&candidate_layout)?.is_empty() {
            return Err(HookError::data(
                "workspace-recovery-handoff-dirty",
                "workspace recovery handoff checkout is dirty",
            ));
        }
        Some(Handoff {
            status: "verified",
            path: candidate.path.clone(),
            branch: candidate.branch.clone().expect("checked branch"),
            head: candidate.head.clone().expect("checked head"),
        })
    } else {
        None
    };
    Ok(ResultPayload {
        schema_version: RESULT_SCHEMA,
        action,
        state: if checkout.dirty_entries.is_empty() {
            "clean-now"
        } else {
            "dirty"
        },
        checkout,
        worktrees: entries,
        worktrees_omitted: 0,
        handoff,
    })
}

fn bounded_payload(mut payload: ResultPayload) -> Result<ResultPayload, HookError> {
    loop {
        let bytes = serde_json::to_vec(&payload).map_err(|_| {
            unavailable(
                "workspace-recovery-output-unavailable",
                "workspace recovery output could not be rendered",
            )
        })?;
        if bytes.len() <= RESULT_MAX_BYTES {
            return Ok(payload);
        }
        if !payload.checkout.dirty_entries.is_empty() {
            let retained = payload.checkout.dirty_entries.len() / 2;
            let omitted = payload.checkout.dirty_entries.len() - retained;
            payload.checkout.dirty_entries.truncate(retained);
            payload.checkout.dirty_entries_omitted = payload
                .checkout
                .dirty_entries_omitted
                .saturating_add(omitted);
            continue;
        }
        if !payload.worktrees.is_empty() {
            let retained = payload.worktrees.len() / 2;
            let omitted = payload.worktrees.len() - retained;
            payload.worktrees.truncate(retained);
            payload.worktrees_omitted = payload.worktrees_omitted.saturating_add(omitted);
            continue;
        }
        return Err(HookError::data(
            "workspace-recovery-output-too-large",
            "workspace recovery output exceeds its bounded contract",
        ));
    }
}

pub(crate) fn run(operation: Operation) -> Result<Outcome, HookError> {
    let payload = bounded_payload(match operation {
        Operation::Inspect => {
            let request: InspectRequest = read_request(INSPECT_SCHEMA)?;
            let layout = validate_request(request.version, &request.cwd)?;
            inspect(layout, "inspect", None)?
        }
        Operation::VerifyHandoff => {
            let request: VerifyRequest = read_request(VERIFY_SCHEMA)?;
            let layout = validate_request(request.version, &request.cwd)?;
            inspect(layout, "verify-handoff", Some(&request.handoff_path))?
        }
    })?;
    let data = serde_json::to_value(&payload).map_err(|_| {
        unavailable(
            "workspace-recovery-output-unavailable",
            "workspace recovery output could not be rendered",
        )
    })?;
    let text = if let Some(handoff) = payload.handoff {
        format!("verified clean managed worktree handoff: {}", handoff.path)
    } else {
        format!(
            "workspace recovery inspection: {} dirty paths, {} worktrees",
            payload
                .checkout
                .dirty_entries
                .len()
                .saturating_add(payload.checkout.dirty_entries_omitted),
            payload
                .worktrees
                .len()
                .saturating_add(payload.worktrees_omitted)
        )
    };
    Ok(Outcome { data, text })
}

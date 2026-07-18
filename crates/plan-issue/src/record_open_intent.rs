use std::fs;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::commands::record::RecordProfile;
use crate::lifecycle_lock::{RecordOpenIdentityKey, record_open_identity_key, record_open_key};
use crate::lifecycle_record::{PayloadProfile, PayloadRole, RecordPayload};
use crate::provider::Repo;
use crate::{CommandError, state};

const RECORD_OPEN_INTENT_SCHEMA: &str = "plan-issue.record-open-intent.v1";
const RECORD_OPEN_INTENT_DIR: &str = "record-open";

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct PersistedRecordOpenIntent {
    schema: String,
    identity: RecordOpenIdentityKey,
    state: RecordOpenIntentState,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "kebab-case", deny_unknown_fields)]
pub(crate) enum RecordOpenIntentState {
    CreateInFlight,
    IssueKnown {
        issue: u64,
    },
    CommentInFlight {
        issue: u64,
        role: PayloadRole,
        expected_payload: RecordPayload,
        expected_fingerprint: String,
    },
}

#[derive(Debug)]
pub(crate) struct RecordOpenIntentStore {
    path: PathBuf,
    identity: RecordOpenIdentityKey,
}

impl RecordOpenIntentStore {
    pub(crate) fn new(
        repo: &Repo,
        profile: RecordProfile,
        source_path: &str,
        source_commit: &str,
    ) -> Self {
        let identity = record_open_identity_key(repo, profile, source_path, source_commit);
        let key = record_open_key(&identity);
        let path = state::state_dir()
            .join("intents")
            .join(RECORD_OPEN_INTENT_DIR)
            .join(format!("{key}.json"));
        Self { path, identity }
    }

    #[cfg(test)]
    pub(crate) fn path(&self) -> &std::path::Path {
        &self.path
    }

    pub(crate) fn load(&self) -> Result<Option<RecordOpenIntentState>, CommandError> {
        let raw = match fs::read(&self.path) {
            Ok(raw) => raw,
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(err) => {
                return Err(CommandError::runtime(
                    "record-open-intent-read-failed",
                    format!(
                        "failed to read record-open intent {}: {err}",
                        self.path.display()
                    ),
                ));
            }
        };
        let persisted: PersistedRecordOpenIntent = serde_json::from_slice(&raw).map_err(|err| {
            CommandError::runtime(
                "record-open-intent-invalid",
                format!(
                    "failed to parse record-open intent {}: {err}",
                    self.path.display()
                ),
            )
        })?;
        if persisted.schema != RECORD_OPEN_INTENT_SCHEMA {
            return Err(self.invalid(format!("unsupported schema `{}`", persisted.schema)));
        }
        if persisted.identity != self.identity {
            return Err(self.invalid(
                "intent identity does not match the canonical record-open key".to_string(),
            ));
        }
        self.validate_state(&persisted.state)?;
        Ok(Some(persisted.state))
    }

    pub(crate) fn persist_create_in_flight(&self) -> Result<(), CommandError> {
        self.persist(RecordOpenIntentState::CreateInFlight)
    }

    pub(crate) fn persist_issue_known(&self, issue: u64) -> Result<(), CommandError> {
        self.persist(RecordOpenIntentState::IssueKnown { issue })
    }

    pub(crate) fn persist_comment_in_flight(
        &self,
        issue: u64,
        expected_payload: &RecordPayload,
    ) -> Result<(), CommandError> {
        self.persist(RecordOpenIntentState::CommentInFlight {
            issue,
            role: expected_payload.role,
            expected_fingerprint: semantic_payload_fingerprint(expected_payload),
            expected_payload: expected_payload.clone(),
        })
    }

    pub(crate) fn clear(&self) -> Result<(), CommandError> {
        match fs::remove_file(&self.path) {
            Ok(()) => sync_parent_directory(&self.path).map_err(|err| {
                CommandError::runtime(
                    "record-open-intent-cleanup-failed",
                    format!(
                        "failed to durably remove converged record-open intent {}: {err}",
                        self.path.display()
                    ),
                )
            }),
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(err) => Err(CommandError::runtime(
                "record-open-intent-cleanup-failed",
                format!(
                    "failed to remove converged record-open intent {}: {err}",
                    self.path.display()
                ),
            )),
        }
    }

    fn persist(&self, state: RecordOpenIntentState) -> Result<(), CommandError> {
        self.validate_state(&state)?;
        let persisted = PersistedRecordOpenIntent {
            schema: RECORD_OPEN_INTENT_SCHEMA.to_string(),
            identity: self.identity.clone(),
            state,
        };
        let raw = serde_json::to_vec_pretty(&persisted).map_err(|err| {
            CommandError::runtime("record-open-intent-write-failed", err.to_string())
        })?;
        nils_common::fs::write_atomic(&self.path, &raw, nils_common::fs::SECRET_FILE_MODE)
            .map_err(|err| {
                CommandError::runtime(
                    "record-open-intent-write-failed",
                    format!(
                        "failed to write record-open intent {}: {err}",
                        self.path.display()
                    ),
                )
            })?;
        sync_file_and_parent(&self.path).map_err(|err| {
            CommandError::runtime(
                "record-open-intent-write-failed",
                format!(
                    "failed to durably persist record-open intent {}: {err}",
                    self.path.display()
                ),
            )
        })
    }

    fn validate_state(&self, state: &RecordOpenIntentState) -> Result<(), CommandError> {
        match state {
            RecordOpenIntentState::CreateInFlight => Ok(()),
            RecordOpenIntentState::IssueKnown { issue } => self.validate_issue(*issue),
            RecordOpenIntentState::CommentInFlight {
                issue,
                role,
                expected_payload,
                expected_fingerprint,
            } => {
                self.validate_issue(*issue)?;
                if *role != expected_payload.role {
                    return Err(self.invalid(
                        "comment role does not match the expected payload role".to_string(),
                    ));
                }
                let expected_profile = match self.identity.profile.as_str() {
                    "tracking" => PayloadProfile::Tracking,
                    "dispatch" => PayloadProfile::Dispatch,
                    other => {
                        return Err(self.invalid(format!(
                            "canonical identity has unsupported profile `{other}`"
                        )));
                    }
                };
                if expected_payload.profile != expected_profile {
                    return Err(self.invalid(
                        "comment payload profile does not match the record-open identity"
                            .to_string(),
                    ));
                }
                let actual_fingerprint = semantic_payload_fingerprint(expected_payload);
                if actual_fingerprint != *expected_fingerprint {
                    return Err(self.invalid(
                        "comment payload fingerprint does not match its semantic payload"
                            .to_string(),
                    ));
                }
                Ok(())
            }
        }
    }

    fn validate_issue(&self, issue: u64) -> Result<(), CommandError> {
        if issue == 0 {
            return Err(self.invalid("known issue number must be non-zero".to_string()));
        }
        Ok(())
    }

    fn invalid(&self, detail: String) -> CommandError {
        CommandError::runtime(
            "record-open-intent-invalid",
            format!(
                "invalid record-open intent {}: {detail}",
                self.path.display()
            ),
        )
    }
}

fn sync_file_and_parent(path: &std::path::Path) -> std::io::Result<()> {
    fs::File::open(path)?.sync_all()?;
    sync_parent_directory(path)
}

#[cfg(unix)]
fn sync_parent_directory(path: &std::path::Path) -> std::io::Result<()> {
    let parent = path.parent().ok_or_else(|| {
        std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            format!("record-open intent path {} has no parent", path.display()),
        )
    })?;
    fs::File::open(parent)?.sync_all()
}

#[cfg(not(unix))]
fn sync_parent_directory(_path: &std::path::Path) -> std::io::Result<()> {
    Ok(())
}

pub(crate) fn semantic_payload_fingerprint(payload: &RecordPayload) -> String {
    let semantic = serde_json::json!({
        "role": payload.role,
        "profile": payload.profile,
        "data": payload.data,
    });
    let raw =
        serde_json::to_vec(&semantic).expect("record payload semantic fingerprint serializes");
    let digest = Sha256::digest(raw);
    digest.iter().map(|byte| format!("{byte:02x}")).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;

    use nils_test_support::{EnvGuard, GlobalStateLock};
    use pretty_assertions::assert_eq;
    use serde_json::json;
    use tempfile::TempDir;

    fn repo(slug: &str, host: Option<&str>) -> Repo {
        Repo {
            provider: crate::provider::Provider::GitHub,
            slug: slug.to_string(),
            host: host.map(str::to_string),
        }
    }

    fn isolate_state(lock: &GlobalStateLock) -> (TempDir, EnvGuard) {
        crate::state::set_state_dir_override(None);
        let tmp = TempDir::new().expect("tmp");
        let guard = EnvGuard::set(lock, "PLAN_ISSUE_HOME", &tmp.path().to_string_lossy());
        (tmp, guard)
    }

    fn payload() -> RecordPayload {
        RecordPayload {
            schema: crate::lifecycle_record::PAYLOAD_SCHEMA_V2.to_string(),
            role: PayloadRole::Source,
            profile: PayloadProfile::Tracking,
            updated_at: Some("2026-07-18T00:00:00Z".to_string()),
            data: json!({"path": "docs/plans/x/source.md", "commit": "abc123"}),
        }
    }

    #[test]
    fn intent_persistence_round_trips_atomically_with_restrictive_mode() {
        let lock = GlobalStateLock::new();
        let (_state, _env) = isolate_state(&lock);
        let store = RecordOpenIntentStore::new(
            &repo("Owner/Repo", Some("ssh.github.com")),
            RecordProfile::Tracking,
            "docs/plans/x/source.md",
            "abc123",
        );

        store.persist_create_in_flight().expect("persist create");
        assert!(matches!(
            store.load().expect("load create"),
            Some(RecordOpenIntentState::CreateInFlight)
        ));
        assert_eq!(
            fs::metadata(store.path())
                .expect("intent metadata")
                .permissions()
                .mode()
                & 0o777,
            nils_common::fs::SECRET_FILE_MODE
        );

        store
            .persist_comment_in_flight(7, &payload())
            .expect("persist comment");
        let Some(RecordOpenIntentState::CommentInFlight {
            issue,
            role,
            expected_payload,
            ..
        }) = store.load().expect("load comment")
        else {
            panic!("expected comment intent");
        };
        assert_eq!(issue, 7);
        assert_eq!(role, PayloadRole::Source);
        assert!(expected_payload.semantically_matches(&payload()));

        store.clear().expect("clear");
        assert!(!store.path().exists());
    }

    #[test]
    fn intent_uses_same_canonical_key_for_provider_aliases() {
        let lock = GlobalStateLock::new();
        let (_state, _env) = isolate_state(&lock);
        let mixed = RecordOpenIntentStore::new(
            &repo("Owner/Repo", Some("ssh.github.com")),
            RecordProfile::Tracking,
            "docs/plans/x/source.md",
            "abc123",
        );
        let canonical = RecordOpenIntentStore::new(
            &repo("owner/repo", Some("github.com")),
            RecordProfile::Tracking,
            "docs/plans/x/source.md",
            "abc123",
        );

        assert_eq!(mixed.path(), canonical.path());
        assert!(
            !mixed.path().to_string_lossy().contains("locks/record-open"),
            "intent journal must not reuse the persistent lock inode"
        );
    }

    #[test]
    fn corrupt_or_mismatched_intent_fails_closed() {
        let lock = GlobalStateLock::new();
        let (_state, _env) = isolate_state(&lock);
        let store = RecordOpenIntentStore::new(
            &repo("owner/repo", Some("github.com")),
            RecordProfile::Tracking,
            "docs/plans/x/source.md",
            "abc123",
        );
        fs::create_dir_all(store.path().parent().expect("parent")).expect("intent parent");
        fs::write(store.path(), b"{broken").expect("corrupt intent");
        assert_eq!(
            store.load().expect_err("corrupt intent must fail").code,
            "record-open-intent-invalid"
        );

        let other = RecordOpenIntentStore::new(
            &repo("owner/repo", Some("github.com")),
            RecordProfile::Tracking,
            "docs/plans/y/source.md",
            "def456",
        );
        let mismatched = PersistedRecordOpenIntent {
            schema: RECORD_OPEN_INTENT_SCHEMA.to_string(),
            identity: other.identity,
            state: RecordOpenIntentState::CreateInFlight,
        };
        fs::write(
            store.path(),
            serde_json::to_vec(&mismatched).expect("serialize mismatch"),
        )
        .expect("mismatched intent");
        assert_eq!(
            store.load().expect_err("mismatch must fail").code,
            "record-open-intent-invalid"
        );
    }
}

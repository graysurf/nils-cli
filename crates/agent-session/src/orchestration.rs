use std::collections::BTreeMap;
use std::fs::{self, File, OpenOptions};
use std::io::{Read, Seek, SeekFrom, Write};
use std::os::fd::{AsRawFd, FromRawFd};
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::{MetadataExt, OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::thread;
use std::time::{Duration, Instant};

use nils_common::fs::{SECRET_FILE_MODE, write_atomic};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};

use crate::{CliContext, CliError, SessionRecord};

#[cfg(test)]
static REGISTRY_SAVE_BYTES_FOR_TEST: std::sync::atomic::AtomicU64 =
    std::sync::atomic::AtomicU64::new(0);
#[cfg(test)]
thread_local! {
    static GROUP_CLEANUP_PROGRESS_BODY_READS_FOR_TEST: std::cell::Cell<u64> =
        const { std::cell::Cell::new(0) };
    static GROUP_CLEANUP_PROGRESS_RECEIPT_DECODES_FOR_TEST: std::cell::Cell<u64> =
        const { std::cell::Cell::new(0) };
    static GROUP_CLEANUP_PROGRESS_COMPACTIONS_FOR_TEST: std::cell::Cell<u64> =
        const { std::cell::Cell::new(0) };
}
#[cfg(test)]
struct GroupCleanupProgressScanHook {
    root: PathBuf,
    scanned: std::sync::Arc<std::sync::Barrier>,
    resume: std::sync::Arc<std::sync::Barrier>,
}
#[cfg(test)]
static GROUP_CLEANUP_PROGRESS_SCAN_HOOK: std::sync::OnceLock<
    std::sync::Mutex<Option<GroupCleanupProgressScanHook>>,
> = std::sync::OnceLock::new();
#[cfg(test)]
struct GroupCleanupProgressEvictionHook {
    path: PathBuf,
    classified: std::sync::Arc<std::sync::Barrier>,
    resume: std::sync::Arc<std::sync::Barrier>,
}
#[cfg(test)]
static GROUP_CLEANUP_PROGRESS_EVICTION_HOOK: std::sync::OnceLock<
    std::sync::Mutex<Option<GroupCleanupProgressEvictionHook>>,
> = std::sync::OnceLock::new();
#[cfg(test)]
struct GroupCleanupProgressRecycleHook {
    path: PathBuf,
    swapped: std::sync::Arc<std::sync::Barrier>,
    resume: std::sync::Arc<std::sync::Barrier>,
}
#[cfg(test)]
static GROUP_CLEANUP_PROGRESS_RECYCLE_HOOK: std::sync::OnceLock<
    std::sync::Mutex<Option<GroupCleanupProgressRecycleHook>>,
> = std::sync::OnceLock::new();
#[cfg(test)]
static GROUP_CLEANUP_PROGRESS_READ_FAILURE: std::sync::OnceLock<std::sync::Mutex<Option<PathBuf>>> =
    std::sync::OnceLock::new();
#[cfg(test)]
static GROUP_CLEANUP_PROGRESS_CRASH_AFTER_RECYCLE: std::sync::OnceLock<
    std::sync::Mutex<Option<PathBuf>>,
> = std::sync::OnceLock::new();
#[cfg(test)]
static GROUP_CLEANUP_PROGRESS_JOURNAL_SYNC_FAILURE: std::sync::OnceLock<
    std::sync::Mutex<Option<PathBuf>>,
> = std::sync::OnceLock::new();
#[cfg(test)]
static GROUP_CLEANUP_PROGRESS_DIRECTORY_SYNC_FAILURE: std::sync::OnceLock<
    std::sync::Mutex<Option<PathBuf>>,
> = std::sync::OnceLock::new();
#[cfg(test)]
struct GroupCleanupProgressJournalHook {
    parent: PathBuf,
    renamed: std::sync::Arc<std::sync::Barrier>,
    resume: std::sync::Arc<std::sync::Barrier>,
}
#[cfg(test)]
static GROUP_CLEANUP_PROGRESS_JOURNAL_HOOK: std::sync::OnceLock<
    std::sync::Mutex<Option<GroupCleanupProgressJournalHook>>,
> = std::sync::OnceLock::new();
#[cfg(test)]
struct GroupCleanupProgressPreExchangeHook {
    parent: PathBuf,
    ready: std::sync::Arc<std::sync::Barrier>,
    resume: std::sync::Arc<std::sync::Barrier>,
}
#[cfg(test)]
static GROUP_CLEANUP_PROGRESS_PRE_EXCHANGE_HOOK: std::sync::OnceLock<
    std::sync::Mutex<Option<GroupCleanupProgressPreExchangeHook>>,
> = std::sync::OnceLock::new();
#[cfg(test)]
struct GroupCleanupProgressRecoveryExchangeHook {
    path: PathBuf,
    ready: std::sync::Arc<std::sync::Barrier>,
    resume: std::sync::Arc<std::sync::Barrier>,
}
#[cfg(test)]
static GROUP_CLEANUP_PROGRESS_RECOVERY_EXCHANGE_HOOK: std::sync::OnceLock<
    std::sync::Mutex<Option<GroupCleanupProgressRecoveryExchangeHook>>,
> = std::sync::OnceLock::new();
#[cfg(test)]
struct GroupCleanupProgressPostVerifyHook {
    path: PathBuf,
    ready: std::sync::Arc<std::sync::Barrier>,
    resume: std::sync::Arc<std::sync::Barrier>,
}
#[cfg(test)]
static GROUP_CLEANUP_PROGRESS_POST_VERIFY_HOOK: std::sync::OnceLock<
    std::sync::Mutex<Option<GroupCleanupProgressPostVerifyHook>>,
> = std::sync::OnceLock::new();
#[cfg(test)]
struct GroupCleanupProgressFinalRenameHook {
    path: PathBuf,
    ready: std::sync::Arc<std::sync::Barrier>,
    resume: std::sync::Arc<std::sync::Barrier>,
}
#[cfg(test)]
static GROUP_CLEANUP_PROGRESS_FINAL_RENAME_HOOK: std::sync::OnceLock<
    std::sync::Mutex<Option<GroupCleanupProgressFinalRenameHook>>,
> = std::sync::OnceLock::new();
#[cfg(test)]
static GROUP_CLEANUP_PROGRESS_CRASH_AFTER_FINAL_RENAME: std::sync::OnceLock<
    std::sync::Mutex<Option<PathBuf>>,
> = std::sync::OnceLock::new();
#[cfg(test)]
struct GroupCleanupProgressPostInstallHook {
    path: PathBuf,
    ready: std::sync::Arc<std::sync::Barrier>,
    resume: std::sync::Arc<std::sync::Barrier>,
}
#[cfg(test)]
static GROUP_CLEANUP_PROGRESS_POST_INSTALL_HOOK: std::sync::OnceLock<
    std::sync::Mutex<Option<GroupCleanupProgressPostInstallHook>>,
> = std::sync::OnceLock::new();
#[cfg(test)]
static GROUP_CLEANUP_PROGRESS_CRASH_AFTER_FINAL_INSTALL: std::sync::OnceLock<
    std::sync::Mutex<Option<PathBuf>>,
> = std::sync::OnceLock::new();
#[cfg(test)]
struct GroupCleanupProgressVisitCounter {
    root: PathBuf,
    visits: std::sync::Arc<std::sync::atomic::AtomicU64>,
}
#[cfg(test)]
static GROUP_CLEANUP_PROGRESS_VISIT_COUNTER: std::sync::OnceLock<
    std::sync::Mutex<Option<GroupCleanupProgressVisitCounter>>,
> = std::sync::OnceLock::new();

#[cfg(test)]
pub(crate) fn reset_registry_save_bytes_for_test() {
    REGISTRY_SAVE_BYTES_FOR_TEST.store(0, std::sync::atomic::Ordering::Release);
}

#[cfg(test)]
pub(crate) fn registry_save_bytes_for_test() -> u64 {
    REGISTRY_SAVE_BYTES_FOR_TEST.load(std::sync::atomic::Ordering::Acquire)
}

#[cfg(test)]
pub(crate) fn reset_group_cleanup_progress_body_reads_for_test() {
    GROUP_CLEANUP_PROGRESS_BODY_READS_FOR_TEST.set(0);
}

#[cfg(test)]
pub(crate) fn group_cleanup_progress_body_reads_for_test() -> u64 {
    GROUP_CLEANUP_PROGRESS_BODY_READS_FOR_TEST.get()
}

#[cfg(test)]
pub(crate) fn reset_group_cleanup_progress_receipt_decodes_for_test() {
    GROUP_CLEANUP_PROGRESS_RECEIPT_DECODES_FOR_TEST.set(0);
}

#[cfg(test)]
pub(crate) fn group_cleanup_progress_receipt_decodes_for_test() -> u64 {
    GROUP_CLEANUP_PROGRESS_RECEIPT_DECODES_FOR_TEST.get()
}

#[cfg(test)]
pub(crate) fn reset_group_cleanup_progress_compactions_for_test() {
    GROUP_CLEANUP_PROGRESS_COMPACTIONS_FOR_TEST.set(0);
}

#[cfg(test)]
pub(crate) fn group_cleanup_progress_compactions_for_test() -> u64 {
    GROUP_CLEANUP_PROGRESS_COMPACTIONS_FOR_TEST.get()
}

#[cfg(test)]
pub(crate) fn install_group_cleanup_progress_scan_hook_for_test(
    root: &Path,
) -> (
    std::sync::Arc<std::sync::Barrier>,
    std::sync::Arc<std::sync::Barrier>,
) {
    let scanned = std::sync::Arc::new(std::sync::Barrier::new(2));
    let resume = std::sync::Arc::new(std::sync::Barrier::new(2));
    let mut slot = GROUP_CLEANUP_PROGRESS_SCAN_HOOK
        .get_or_init(|| std::sync::Mutex::new(None))
        .lock()
        .expect("group cleanup progress scan hook");
    assert!(slot.is_none(), "group cleanup progress scan hook is busy");
    *slot = Some(GroupCleanupProgressScanHook {
        root: root.to_path_buf(),
        scanned: scanned.clone(),
        resume: resume.clone(),
    });
    (scanned, resume)
}

#[cfg(test)]
fn pause_group_cleanup_progress_after_scan_for_test(root: &Path) {
    let hook = {
        let mut slot = GROUP_CLEANUP_PROGRESS_SCAN_HOOK
            .get_or_init(|| std::sync::Mutex::new(None))
            .lock()
            .expect("group cleanup progress scan hook");
        if slot.as_ref().is_some_and(|hook| hook.root == root) {
            slot.take()
        } else {
            None
        }
    };
    if let Some(hook) = hook {
        hook.scanned.wait();
        hook.resume.wait();
    }
}

#[cfg(test)]
pub(crate) fn install_group_cleanup_progress_eviction_hook_for_test(
    path: &Path,
) -> (
    std::sync::Arc<std::sync::Barrier>,
    std::sync::Arc<std::sync::Barrier>,
) {
    let classified = std::sync::Arc::new(std::sync::Barrier::new(2));
    let resume = std::sync::Arc::new(std::sync::Barrier::new(2));
    let mut slot = GROUP_CLEANUP_PROGRESS_EVICTION_HOOK
        .get_or_init(|| std::sync::Mutex::new(None))
        .lock()
        .expect("group cleanup progress eviction hook");
    assert!(
        slot.is_none(),
        "group cleanup progress eviction hook is busy"
    );
    *slot = Some(GroupCleanupProgressEvictionHook {
        path: path.to_path_buf(),
        classified: classified.clone(),
        resume: resume.clone(),
    });
    (classified, resume)
}

#[cfg(test)]
fn pause_group_cleanup_progress_before_eviction_for_test(path: &Path) {
    let hook = {
        let mut slot = GROUP_CLEANUP_PROGRESS_EVICTION_HOOK
            .get_or_init(|| std::sync::Mutex::new(None))
            .lock()
            .expect("group cleanup progress eviction hook");
        if slot.as_ref().is_some_and(|hook| hook.path == path) {
            slot.take()
        } else {
            None
        }
    };
    if let Some(hook) = hook {
        hook.classified.wait();
        hook.resume.wait();
    }
}

#[cfg(test)]
pub(crate) fn fail_group_cleanup_progress_read_for_test(path: &Path) {
    let mut slot = GROUP_CLEANUP_PROGRESS_READ_FAILURE
        .get_or_init(|| std::sync::Mutex::new(None))
        .lock()
        .expect("group cleanup progress read failure");
    assert!(
        slot.is_none(),
        "group cleanup progress read failure is busy"
    );
    *slot = Some(path.to_path_buf());
}

#[cfg(test)]
fn group_cleanup_progress_read_fails_for_test(path: &Path) -> bool {
    let mut slot = GROUP_CLEANUP_PROGRESS_READ_FAILURE
        .get_or_init(|| std::sync::Mutex::new(None))
        .lock()
        .expect("group cleanup progress read failure");
    if slot.as_ref().is_some_and(|candidate| candidate == path) {
        slot.take();
        true
    } else {
        false
    }
}

#[cfg(test)]
pub(crate) fn fail_group_cleanup_progress_after_recycle_for_test(context: &CliContext) {
    let path =
        group_cleanup_progress_recycle_path(context).expect("group cleanup progress recycle path");
    let mut target = GROUP_CLEANUP_PROGRESS_CRASH_AFTER_RECYCLE
        .get_or_init(|| std::sync::Mutex::new(None))
        .lock()
        .expect("group cleanup progress crash target");
    assert!(
        target.is_none(),
        "group cleanup progress crash target is busy"
    );
    *target = Some(path);
}

#[cfg(test)]
pub(crate) fn fail_group_cleanup_progress_journal_sync_for_test(context: &CliContext) {
    let path =
        group_cleanup_progress_recycle_path(context).expect("group cleanup progress recycle path");
    let mut target = GROUP_CLEANUP_PROGRESS_JOURNAL_SYNC_FAILURE
        .get_or_init(|| std::sync::Mutex::new(None))
        .lock()
        .expect("group cleanup progress journal sync target");
    assert!(
        target.is_none(),
        "group cleanup progress journal sync target is busy"
    );
    *target = path.parent().map(Path::to_path_buf);
}

#[cfg(test)]
pub(crate) fn fail_group_cleanup_progress_directory_sync_for_test(context: &CliContext) {
    let path = ensure_orchestration_root(context)
        .expect("orchestration root")
        .join(GROUP_CLEANUP_PROGRESS_DIR);
    let mut target = GROUP_CLEANUP_PROGRESS_DIRECTORY_SYNC_FAILURE
        .get_or_init(|| std::sync::Mutex::new(None))
        .lock()
        .expect("group cleanup progress directory sync target");
    assert!(
        target.is_none(),
        "group cleanup progress directory sync target is busy"
    );
    *target = Some(path);
}

#[cfg(test)]
fn group_cleanup_progress_journal_sync_fails_for_test(parent: &Path) -> bool {
    let mut target = GROUP_CLEANUP_PROGRESS_JOURNAL_SYNC_FAILURE
        .get_or_init(|| std::sync::Mutex::new(None))
        .lock()
        .expect("group cleanup progress journal sync target");
    if target.as_deref() == Some(parent) {
        target.take();
        true
    } else {
        false
    }
}

#[cfg(test)]
fn group_cleanup_progress_directory_sync_fails_for_test(parent: &Path) -> bool {
    let mut target = GROUP_CLEANUP_PROGRESS_DIRECTORY_SYNC_FAILURE
        .get_or_init(|| std::sync::Mutex::new(None))
        .lock()
        .expect("group cleanup progress directory sync target");
    if target.as_deref() == Some(parent) {
        target.take();
        true
    } else {
        false
    }
}

#[cfg(test)]
pub(crate) fn install_group_cleanup_progress_journal_hook_for_test(
    context: &CliContext,
) -> (
    std::sync::Arc<std::sync::Barrier>,
    std::sync::Arc<std::sync::Barrier>,
    PathBuf,
) {
    let path = group_cleanup_progress_recycle_journal_path(context)
        .expect("group cleanup progress recycle journal path");
    let parent = path.parent().expect("recycle journal parent").to_path_buf();
    let renamed = std::sync::Arc::new(std::sync::Barrier::new(2));
    let resume = std::sync::Arc::new(std::sync::Barrier::new(2));
    let mut slot = GROUP_CLEANUP_PROGRESS_JOURNAL_HOOK
        .get_or_init(|| std::sync::Mutex::new(None))
        .lock()
        .expect("group cleanup progress journal hook");
    assert!(
        slot.is_none(),
        "group cleanup progress journal hook is busy"
    );
    *slot = Some(GroupCleanupProgressJournalHook {
        parent: parent.clone(),
        renamed: renamed.clone(),
        resume: resume.clone(),
    });
    (renamed, resume, parent)
}

#[cfg(test)]
fn pause_group_cleanup_progress_journal_after_rename_for_test(parent: &Path) {
    let hook = {
        let mut slot = GROUP_CLEANUP_PROGRESS_JOURNAL_HOOK
            .get_or_init(|| std::sync::Mutex::new(None))
            .lock()
            .expect("group cleanup progress journal hook");
        if slot.as_ref().is_some_and(|hook| hook.parent == parent) {
            slot.take()
        } else {
            None
        }
    };
    if let Some(hook) = hook {
        hook.renamed.wait();
        hook.resume.wait();
    }
}

#[cfg(test)]
pub(crate) fn store_idle_group_cleanup_progress_recycle_journal_for_test(
    context: &CliContext,
) -> Result<(), CliError> {
    store_idle_group_cleanup_progress_recycle_journal(context)
}

#[cfg(test)]
pub(crate) fn install_group_cleanup_progress_pre_exchange_hook_for_test(
    context: &CliContext,
) -> (
    std::sync::Arc<std::sync::Barrier>,
    std::sync::Arc<std::sync::Barrier>,
    PathBuf,
) {
    let path = group_cleanup_progress_recycle_journal_path(context)
        .expect("group cleanup progress recycle journal path");
    let parent = path.parent().expect("recycle journal parent").to_path_buf();
    let ready = std::sync::Arc::new(std::sync::Barrier::new(2));
    let resume = std::sync::Arc::new(std::sync::Barrier::new(2));
    let mut slot = GROUP_CLEANUP_PROGRESS_PRE_EXCHANGE_HOOK
        .get_or_init(|| std::sync::Mutex::new(None))
        .lock()
        .expect("group cleanup progress pre-exchange hook");
    assert!(
        slot.is_none(),
        "group cleanup progress pre-exchange hook is busy"
    );
    *slot = Some(GroupCleanupProgressPreExchangeHook {
        parent: parent.clone(),
        ready: ready.clone(),
        resume: resume.clone(),
    });
    (ready, resume, parent)
}

#[cfg(test)]
fn pause_group_cleanup_progress_before_exchange_for_test(parent: &Path) {
    let hook = {
        let mut slot = GROUP_CLEANUP_PROGRESS_PRE_EXCHANGE_HOOK
            .get_or_init(|| std::sync::Mutex::new(None))
            .lock()
            .expect("group cleanup progress pre-exchange hook");
        if slot.as_ref().is_some_and(|hook| hook.parent == parent) {
            slot.take()
        } else {
            None
        }
    };
    if let Some(hook) = hook {
        hook.ready.wait();
        hook.resume.wait();
    }
}

#[cfg(test)]
pub(crate) fn install_group_cleanup_progress_recovery_exchange_hook_for_test(
    path: &Path,
) -> (
    std::sync::Arc<std::sync::Barrier>,
    std::sync::Arc<std::sync::Barrier>,
) {
    let ready = std::sync::Arc::new(std::sync::Barrier::new(2));
    let resume = std::sync::Arc::new(std::sync::Barrier::new(2));
    let mut slot = GROUP_CLEANUP_PROGRESS_RECOVERY_EXCHANGE_HOOK
        .get_or_init(|| std::sync::Mutex::new(None))
        .lock()
        .expect("group cleanup progress recovery exchange hook");
    assert!(
        slot.is_none(),
        "group cleanup progress recovery exchange hook is busy"
    );
    *slot = Some(GroupCleanupProgressRecoveryExchangeHook {
        path: path.to_path_buf(),
        ready: ready.clone(),
        resume: resume.clone(),
    });
    (ready, resume)
}

#[cfg(test)]
fn pause_group_cleanup_progress_recovery_before_exchange_for_test(path: &Path) {
    let hook = {
        let mut slot = GROUP_CLEANUP_PROGRESS_RECOVERY_EXCHANGE_HOOK
            .get_or_init(|| std::sync::Mutex::new(None))
            .lock()
            .expect("group cleanup progress recovery exchange hook");
        if slot.as_ref().is_some_and(|hook| hook.path == path) {
            slot.take()
        } else {
            None
        }
    };
    if let Some(hook) = hook {
        hook.ready.wait();
        hook.resume.wait();
    }
}

#[cfg(test)]
pub(crate) fn install_group_cleanup_progress_post_verify_hook_for_test(
    path: &Path,
) -> (
    std::sync::Arc<std::sync::Barrier>,
    std::sync::Arc<std::sync::Barrier>,
) {
    let ready = std::sync::Arc::new(std::sync::Barrier::new(2));
    let resume = std::sync::Arc::new(std::sync::Barrier::new(2));
    let mut slot = GROUP_CLEANUP_PROGRESS_POST_VERIFY_HOOK
        .get_or_init(|| std::sync::Mutex::new(None))
        .lock()
        .expect("group cleanup progress post-verify hook");
    assert!(
        slot.is_none(),
        "group cleanup progress post-verify hook is busy"
    );
    *slot = Some(GroupCleanupProgressPostVerifyHook {
        path: path.to_path_buf(),
        ready: ready.clone(),
        resume: resume.clone(),
    });
    (ready, resume)
}

#[cfg(test)]
fn pause_group_cleanup_progress_after_source_verify_for_test(path: &Path) {
    let hook = {
        let mut slot = GROUP_CLEANUP_PROGRESS_POST_VERIFY_HOOK
            .get_or_init(|| std::sync::Mutex::new(None))
            .lock()
            .expect("group cleanup progress post-verify hook");
        if slot.as_ref().is_some_and(|hook| hook.path == path) {
            slot.take()
        } else {
            None
        }
    };
    if let Some(hook) = hook {
        hook.ready.wait();
        hook.resume.wait();
    }
}

#[cfg(test)]
pub(crate) fn install_group_cleanup_progress_final_rename_hook_for_test(
    path: &Path,
) -> (
    std::sync::Arc<std::sync::Barrier>,
    std::sync::Arc<std::sync::Barrier>,
) {
    let ready = std::sync::Arc::new(std::sync::Barrier::new(2));
    let resume = std::sync::Arc::new(std::sync::Barrier::new(2));
    let mut slot = GROUP_CLEANUP_PROGRESS_FINAL_RENAME_HOOK
        .get_or_init(|| std::sync::Mutex::new(None))
        .lock()
        .expect("group cleanup progress final rename hook");
    assert!(
        slot.is_none(),
        "group cleanup progress final rename hook is busy"
    );
    *slot = Some(GroupCleanupProgressFinalRenameHook {
        path: path.to_path_buf(),
        ready: ready.clone(),
        resume: resume.clone(),
    });
    (ready, resume)
}

#[cfg(test)]
fn pause_group_cleanup_progress_after_final_rename_for_test(path: &Path) {
    let hook = {
        let mut slot = GROUP_CLEANUP_PROGRESS_FINAL_RENAME_HOOK
            .get_or_init(|| std::sync::Mutex::new(None))
            .lock()
            .expect("group cleanup progress final rename hook");
        if slot.as_ref().is_some_and(|hook| hook.path == path) {
            slot.take()
        } else {
            None
        }
    };
    if let Some(hook) = hook {
        hook.ready.wait();
        hook.resume.wait();
    }
}

#[cfg(test)]
pub(crate) fn fail_group_cleanup_progress_after_final_rename_for_test(path: &Path) {
    let mut target = GROUP_CLEANUP_PROGRESS_CRASH_AFTER_FINAL_RENAME
        .get_or_init(|| std::sync::Mutex::new(None))
        .lock()
        .expect("group cleanup progress post-rename crash target");
    assert!(
        target.is_none(),
        "group cleanup progress post-rename crash target is busy"
    );
    *target = Some(path.to_path_buf());
}

#[cfg(test)]
fn group_cleanup_progress_crashes_after_final_rename_for_test(path: &Path) -> bool {
    let mut target = GROUP_CLEANUP_PROGRESS_CRASH_AFTER_FINAL_RENAME
        .get_or_init(|| std::sync::Mutex::new(None))
        .lock()
        .expect("group cleanup progress post-rename crash target");
    if target.as_deref() == Some(path) {
        target.take();
        true
    } else {
        false
    }
}

#[cfg(test)]
pub(crate) fn install_group_cleanup_progress_post_install_hook_for_test(
    path: &Path,
) -> (
    std::sync::Arc<std::sync::Barrier>,
    std::sync::Arc<std::sync::Barrier>,
) {
    let ready = std::sync::Arc::new(std::sync::Barrier::new(2));
    let resume = std::sync::Arc::new(std::sync::Barrier::new(2));
    let mut slot = GROUP_CLEANUP_PROGRESS_POST_INSTALL_HOOK
        .get_or_init(|| std::sync::Mutex::new(None))
        .lock()
        .expect("group cleanup progress post-install hook");
    assert!(
        slot.is_none(),
        "group cleanup progress post-install hook is busy"
    );
    *slot = Some(GroupCleanupProgressPostInstallHook {
        path: path.to_path_buf(),
        ready: ready.clone(),
        resume: resume.clone(),
    });
    (ready, resume)
}

#[cfg(test)]
pub(crate) fn fail_group_cleanup_progress_after_final_install_for_test(path: &Path) {
    let mut target = GROUP_CLEANUP_PROGRESS_CRASH_AFTER_FINAL_INSTALL
        .get_or_init(|| std::sync::Mutex::new(None))
        .lock()
        .expect("group cleanup progress post-install crash target");
    assert!(
        target.is_none(),
        "group cleanup progress post-install crash target is busy"
    );
    *target = Some(path.to_path_buf());
}

#[cfg(test)]
fn pause_group_cleanup_progress_after_install_for_test(path: &Path) {
    let hook = {
        let mut slot = GROUP_CLEANUP_PROGRESS_POST_INSTALL_HOOK
            .get_or_init(|| std::sync::Mutex::new(None))
            .lock()
            .expect("group cleanup progress post-install hook");
        if slot.as_ref().is_some_and(|hook| hook.path == path) {
            slot.take()
        } else {
            None
        }
    };
    if let Some(hook) = hook {
        hook.ready.wait();
        hook.resume.wait();
    }
}

#[cfg(test)]
fn group_cleanup_progress_crashes_after_final_install_for_test(path: &Path) -> bool {
    let mut target = GROUP_CLEANUP_PROGRESS_CRASH_AFTER_FINAL_INSTALL
        .get_or_init(|| std::sync::Mutex::new(None))
        .lock()
        .expect("group cleanup progress post-install crash target");
    if target.as_deref() == Some(path) {
        target.take();
        true
    } else {
        false
    }
}

#[cfg(test)]
fn group_cleanup_progress_crashes_after_recycle_for_test(path: &Path) -> bool {
    let mut target = GROUP_CLEANUP_PROGRESS_CRASH_AFTER_RECYCLE
        .get_or_init(|| std::sync::Mutex::new(None))
        .lock()
        .expect("group cleanup progress crash target");
    if target.as_deref() == Some(path) {
        target.take();
        true
    } else {
        false
    }
}

#[cfg(test)]
pub(crate) fn install_group_cleanup_progress_recycle_hook_for_test(
    context: &CliContext,
) -> (
    std::sync::Arc<std::sync::Barrier>,
    std::sync::Arc<std::sync::Barrier>,
    PathBuf,
) {
    let path =
        group_cleanup_progress_recycle_path(context).expect("group cleanup progress recycle path");
    let swapped = std::sync::Arc::new(std::sync::Barrier::new(2));
    let resume = std::sync::Arc::new(std::sync::Barrier::new(2));
    let mut slot = GROUP_CLEANUP_PROGRESS_RECYCLE_HOOK
        .get_or_init(|| std::sync::Mutex::new(None))
        .lock()
        .expect("group cleanup progress recycle hook");
    assert!(
        slot.is_none(),
        "group cleanup progress recycle hook is busy"
    );
    *slot = Some(GroupCleanupProgressRecycleHook {
        path: path.clone(),
        swapped: swapped.clone(),
        resume: resume.clone(),
    });
    (swapped, resume, path)
}

#[cfg(test)]
fn pause_group_cleanup_progress_after_recycle_for_test(path: &Path) {
    let hook = {
        let mut slot = GROUP_CLEANUP_PROGRESS_RECYCLE_HOOK
            .get_or_init(|| std::sync::Mutex::new(None))
            .lock()
            .expect("group cleanup progress recycle hook");
        if slot.as_ref().is_some_and(|hook| hook.path == path) {
            slot.take()
        } else {
            None
        }
    };
    if let Some(hook) = hook {
        hook.swapped.wait();
        hook.resume.wait();
    }
}

#[cfg(test)]
pub(crate) fn seed_group_cleanup_progress_recycle_residue_for_test(
    context: &CliContext,
    bytes: &[u8],
) -> Result<(), CliError> {
    let path = group_cleanup_progress_recycle_path(context)?;
    write_atomic(&path, bytes, SECRET_FILE_MODE).map_err(|_| store_unavailable())
}

#[cfg(test)]
pub(crate) fn group_cleanup_progress_recycle_slot_is_retired_for_test(
    context: &CliContext,
) -> bool {
    let Ok(path) = group_cleanup_progress_recycle_path(context) else {
        return false;
    };
    read_private_bounded_file_with_limit(
        &path,
        MAX_REGISTRY_BYTES,
        "group cleanup progress recycle permissions are unsafe",
        "group cleanup progress recycle exceeds byte limit",
        "group cleanup progress recycle changed while it was being read",
    )
    .is_ok_and(|snapshot| {
        snapshot.is_some_and(|snapshot| snapshot.bytes == group_cleanup_progress_retired_receipt())
    })
}

#[cfg(test)]
pub(crate) fn install_group_cleanup_progress_visit_counter_for_test(
    root: &Path,
) -> std::sync::Arc<std::sync::atomic::AtomicU64> {
    let visits = std::sync::Arc::new(std::sync::atomic::AtomicU64::new(0));
    let mut slot = GROUP_CLEANUP_PROGRESS_VISIT_COUNTER
        .get_or_init(|| std::sync::Mutex::new(None))
        .lock()
        .expect("group cleanup progress visit counter");
    assert!(
        slot.is_none(),
        "group cleanup progress visit counter is busy"
    );
    *slot = Some(GroupCleanupProgressVisitCounter {
        root: root.to_path_buf(),
        visits: visits.clone(),
    });
    visits
}

#[cfg(test)]
pub(crate) fn clear_group_cleanup_progress_visit_counter_for_test() {
    *GROUP_CLEANUP_PROGRESS_VISIT_COUNTER
        .get_or_init(|| std::sync::Mutex::new(None))
        .lock()
        .expect("group cleanup progress visit counter") = None;
}

#[cfg(test)]
fn count_group_cleanup_progress_visit_for_test(root: &Path) {
    let slot = GROUP_CLEANUP_PROGRESS_VISIT_COUNTER
        .get_or_init(|| std::sync::Mutex::new(None))
        .lock()
        .expect("group cleanup progress visit counter");
    if let Some(counter) = slot.as_ref().filter(|counter| counter.root == root) {
        counter
            .visits
            .fetch_add(1, std::sync::atomic::Ordering::AcqRel);
    }
}

#[cfg(test)]
type PrivateReadReplacementHook = Box<dyn FnOnce(&Path)>;

#[cfg(test)]
thread_local! {
    static PRIVATE_READ_REPLACEMENT_HOOK: std::cell::RefCell<Option<PrivateReadReplacementHook>> =
        std::cell::RefCell::new(None);
}

#[cfg(test)]
fn replace_private_read_path_for_test(path: &Path) {
    PRIVATE_READ_REPLACEMENT_HOOK.with(|slot| {
        if let Some(hook) = slot.borrow_mut().take() {
            hook(path);
        }
    });
}

pub(crate) const REGISTRY_SCHEMA: &str = "agent-session.orchestration-registry.v3";
const LEGACY_REGISTRY_V2_SCHEMA: &str = "agent-session.orchestration-registry.v2";
const LEGACY_REGISTRY_V1_SCHEMA: &str = "agent-session.orchestration-registry.v1";
pub(crate) const RUN_SCHEMA: &str = "agent-session.orchestration-run.v1";
pub(crate) const ASSIGNMENT_SCHEMA: &str = "agent-session.orchestration-assignment.v3";
const LEGACY_ASSIGNMENT_V2_SCHEMA: &str = "agent-session.orchestration-assignment.v2";
const LEGACY_ASSIGNMENT_V1_SCHEMA: &str = "agent-session.orchestration-assignment.v1";
pub(crate) const SESSION_PROJECTION_SCHEMA: &str = "agent-session.session-orchestration.v1";
pub(crate) const PACKET_SCHEMA: &str = "main-agent.objective-packet.v1";
pub(crate) const ASSIGNMENT_INPUT_SCHEMA: &str = "main-agent.assignment-input.v1";
pub(crate) const CHECKPOINT_INPUT_SCHEMA: &str = "main-agent.checkpoint-input.v1";
pub(crate) const SUBMIT_RECOVERY_SCHEMA: &str = "main-agent.submit-recovery.v1";
pub(crate) const WORKER_QUARANTINE_SCHEMA: &str = "main-agent.worker-quarantine.v1";
pub(crate) const GROUP_CLEANUP_PROGRESS_RECEIPT_SCHEMA: &str =
    "agent-session.main-agent-group-cleanup-receipt.v2";
pub(crate) const GROUP_CLEANUP_PROGRESS_RECEIPT_V1_SCHEMA: &str =
    "agent-session.main-agent-group-cleanup-receipt.v1";
pub(crate) const ACCOUNT_HANDOFF_RESERVATION_SCHEMA: &str =
    "main-agent.account-handoff-reservation.v3";
pub(crate) const LEGACY_ACCOUNT_HANDOFF_RESERVATION_V2_SCHEMA: &str =
    "main-agent.account-handoff-reservation.v2";
pub(crate) const LEGACY_ACCOUNT_HANDOFF_RESERVATION_SCHEMA: &str =
    "main-agent.account-handoff-reservation.v1";
const SESSION_AUTHORITY_QUARANTINE_SCHEMA: &str = "agent-session.worker-authority-quarantine.v1";
const SESSION_GROUP_CLEANUP_FENCE_SCHEMA: &str = "agent-session.group-cleanup-fence.v1";

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct GroupCleanupProgressReceipt {
    pub(crate) schema_version: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) requested_session_id: Option<String>,
    pub(crate) principal_session_id: String,
    pub(crate) principal_incarnation: String,
    pub(crate) idempotency_key: String,
    pub(crate) request_digest: String,
    pub(crate) outcome: Value,
}

pub(crate) struct GroupCleanupSelectorBinding<'a> {
    pub(crate) schema_version: &'a str,
    pub(crate) requested_session_id: Option<&'a str>,
    pub(crate) stored_principal_session_id: &'a str,
    pub(crate) canonical_session_id: &'a str,
    pub(crate) stored_incarnation: &'a str,
    pub(crate) canonical_incarnation: &'a str,
    pub(crate) expected_session_id: &'a str,
    pub(crate) expected_incarnation: &'a str,
}

impl GroupCleanupSelectorBinding<'_> {
    pub(crate) fn is_exact(&self) -> bool {
        if self.stored_incarnation != self.expected_incarnation
            || self.canonical_incarnation != self.expected_incarnation
        {
            return false;
        }
        match self.schema_version {
            GROUP_CLEANUP_PROGRESS_RECEIPT_V1_SCHEMA => {
                self.requested_session_id.is_none()
                    && self.stored_principal_session_id == self.expected_session_id
            }
            GROUP_CLEANUP_PROGRESS_RECEIPT_SCHEMA => {
                self.requested_session_id == Some(self.expected_session_id)
                    && self.stored_principal_session_id == self.canonical_session_id
            }
            _ => false,
        }
    }
}

const GROUP_CLEANUP_PROGRESS_RECYCLE_JOURNAL_SCHEMA: &str =
    "agent-session.group-cleanup-progress-recycle.v1";
const GROUP_CLEANUP_PROGRESS_RECYCLE_PHASE_PREPARED: &str = "prepared";
const GROUP_CLEANUP_PROGRESS_RECYCLE_PHASE_INSTALLED: &str = "installed";

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct GroupCleanupProgressSnapshotIdentity {
    device: u64,
    inode: u64,
    length: u64,
    modified_seconds: i64,
    modified_nanoseconds: i64,
    sha256: String,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct GroupCleanupProgressRecycleJournalWire {
    schema_version: String,
    state: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    phase: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    source_key: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    current_key: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    source: Option<GroupCleanupProgressSnapshotIdentity>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    prepared: Option<GroupCleanupProgressSnapshotIdentity>,
}

enum GroupCleanupProgressRecycleState {
    Idle,
    Prepared {
        source_key: String,
        current_key: Option<String>,
        source: GroupCleanupProgressSnapshotIdentity,
        prepared: GroupCleanupProgressSnapshotIdentity,
    },
    Installed {
        source_key: String,
        current_key: String,
        source: GroupCleanupProgressSnapshotIdentity,
        prepared: GroupCleanupProgressSnapshotIdentity,
    },
}

impl GroupCleanupProgressRecycleJournalWire {
    fn into_state(self) -> Result<GroupCleanupProgressRecycleState, CliError> {
        if self.schema_version != GROUP_CLEANUP_PROGRESS_RECYCLE_JOURNAL_SCHEMA {
            return Err(store_unavailable());
        }
        if self.state == "idle"
            && self.phase.is_none()
            && self.source_key.is_none()
            && self.current_key.is_none()
            && self.source.is_none()
            && self.prepared.is_none()
        {
            return Ok(GroupCleanupProgressRecycleState::Idle);
        }
        if self.state != "active" {
            return Err(store_unavailable());
        }
        let source_key = self.source_key.ok_or_else(store_unavailable)?;
        let source = self.source.ok_or_else(store_unavailable)?;
        let prepared = self.prepared.ok_or_else(store_unavailable)?;
        match self.phase.as_deref() {
            None | Some(GROUP_CLEANUP_PROGRESS_RECYCLE_PHASE_PREPARED) => {
                Ok(GroupCleanupProgressRecycleState::Prepared {
                    source_key,
                    current_key: self.current_key,
                    source,
                    prepared,
                })
            }
            Some(GROUP_CLEANUP_PROGRESS_RECYCLE_PHASE_INSTALLED) => {
                Ok(GroupCleanupProgressRecycleState::Installed {
                    source_key,
                    current_key: self.current_key.ok_or_else(store_unavailable)?,
                    source,
                    prepared,
                })
            }
            Some(_) => Err(store_unavailable()),
        }
    }
}

pub(crate) fn decode_group_cleanup_progress_receipt(
    bytes: &[u8],
) -> Result<GroupCleanupProgressReceipt, ()> {
    let value = parse_group_cleanup_progress_receipt_value(bytes)?;
    decode_group_cleanup_progress_receipt_value(value)
}

fn parse_group_cleanup_progress_receipt_value(bytes: &[u8]) -> Result<Value, ()> {
    #[cfg(test)]
    GROUP_CLEANUP_PROGRESS_RECEIPT_DECODES_FOR_TEST.set(
        GROUP_CLEANUP_PROGRESS_RECEIPT_DECODES_FOR_TEST
            .get()
            .saturating_add(1),
    );
    serde_json::from_slice::<Value>(bytes).map_err(|_| ())
}

fn decode_group_cleanup_progress_receipt_value(
    value: Value,
) -> Result<GroupCleanupProgressReceipt, ()> {
    let object = value.as_object().ok_or(())?;
    match value["schema_version"].as_str() {
        Some(GROUP_CLEANUP_PROGRESS_RECEIPT_V1_SCHEMA)
            if !object.contains_key("requested_session_id") => {}
        Some(GROUP_CLEANUP_PROGRESS_RECEIPT_SCHEMA)
            if value["requested_session_id"]
                .as_str()
                .is_some_and(|requested| crate::validate_id(requested).is_ok()) => {}
        _ => return Err(()),
    }
    let principal = value["principal_session_id"].as_str().ok_or(())?;
    crate::validate_id(principal).map_err(|_| ())?;
    let incarnation = value["principal_incarnation"].as_str().ok_or(())?;
    validate_slug("main session incarnation", incarnation, 128).map_err(|_| ())?;
    serde_json::from_value(value).map_err(|_| ())
}

const ORCHESTRATION_DIR: &str = "orchestration";
const REGISTRY_FILE: &str = "registry.json";
const REGISTRY_V2_ROLLBACK_FILE: &str = "registry.v2.rollback.json";
const REGISTRY_V1_ROLLBACK_FILE: &str = "registry.v1.rollback.json";
const REGISTRY_LOCK: &str = "registry.lock";
const PACKETS_DIR: &str = "packets";
const GROUP_CLEANUP_PROGRESS_DIR: &str = "group-cleanup-progress";
const GROUP_CLEANUP_PROGRESS_RECYCLE_DIR: &str = "group-cleanup-progress-recycle";
const GROUP_CLEANUP_PROGRESS_RECYCLE_FILE: &str = "slot";
const GROUP_CLEANUP_PROGRESS_RECYCLE_JOURNAL_FILE: &str = "journal.json";
const GROUP_CLEANUP_PROGRESS_LOCK: &str = "group-cleanup-progress.lock";
const MAX_REGISTRY_BYTES: u64 = 4 * 1024 * 1024;
const MAX_GROUP_CLEANUP_PROGRESS_FILES: usize = 128;
const MAX_GROUP_CLEANUP_PROGRESS_BYTES: u64 = 16 * 1024 * 1024;
const MAX_SESSION_AUTHORITY_QUARANTINE_BYTES: u64 = 64 * 1024;
const SESSION_AUTHORITY_QUARANTINE_FILE: &str = "authority-quarantine.json";
const SESSION_GROUP_CLEANUP_FENCE_FILE: &str = "group-cleanup-fence.json";
const LOCK_TIMEOUT: Duration = Duration::from_secs(2);

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub(crate) struct SessionRef {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub machine: Option<String>,
    pub session_id: String,
    pub session_incarnation: String,
    pub session_created_at: String,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub(crate) struct RunCheckpoint {
    pub revision: u64,
    pub summary: String,
    pub next_action: String,
    pub updated_at: String,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct RunRecord {
    pub schema_version: String,
    pub run_id: String,
    pub revision: u64,
    pub state: String,
    pub tier: String,
    pub objective_summary: String,
    pub objective_packet_digest: String,
    pub controller: SessionRef,
    #[serde(default)]
    pub durable_refs: Vec<String>,
    /// Ephemeral runs are created by `main-agent quick` and auto-close once
    /// their last assignment's worker is torn down, so a fast-path caller never
    /// runs an explicit `close`.
    #[serde(default)]
    pub ephemeral: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub checkpoint: Option<RunCheckpoint>,
    pub created_at: String,
    pub updated_at: String,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub(crate) struct TimedRelationship {
    pub session: SessionRef,
    pub expires_at: String,
    pub expires_at_epoch: i64,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct SubmitRecoveryRecord {
    pub schema_version: String,
    pub attempt_id: String,
    #[serde(default = "default_submit_recovery_origin")]
    pub origin: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub run_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub controller: Option<SessionRef>,
    pub session_incarnation: String,
    pub reserved_revision: u64,
    pub state: String,
    pub attempt_count: u8,
    pub result: String,
    pub attempted_at: String,
    pub updated_at: String,
}

fn default_submit_recovery_origin() -> String {
    "explicit".to_string()
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub(crate) struct WorkerQuarantineRecord {
    pub schema_version: String,
    pub worker: SessionRef,
    pub reason: String,
    pub runtime_identity_digest: String,
    pub created_at: String,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub(crate) struct AccountHandoffReservationRecord {
    pub schema_version: String,
    pub request_digest: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reservation_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub account_intent_id: Option<String>,
    pub run_id: String,
    pub controller: SessionRef,
    pub worker: SessionRef,
    pub reserved_revision: u64,
    pub account: String,
    pub created_at: String,
    pub updated_at: String,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
struct SessionAuthorityQuarantine {
    schema_version: String,
    assignment_id: String,
    assignment_revision: u64,
    quarantine: WorkerQuarantineRecord,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub(crate) struct SessionGroupCleanupFence {
    schema_version: String,
    pub(crate) worker: SessionRef,
    main: SessionRef,
    run_id: String,
    plan_digest: String,
    created_at: String,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct AssignmentRecord {
    pub schema_version: String,
    pub assignment_id: String,
    pub run_id: String,
    pub revision: u64,
    pub state: String,
    pub task_summary: String,
    pub private_packet_digest: String,
    pub primary_manager: SessionRef,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub worker: Option<SessionRef>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub previous_worker: Option<SessionRef>,
    #[serde(default)]
    pub collaborators: Vec<SessionRef>,
    #[serde(default)]
    pub borrowed_by: Vec<TimedRelationship>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub repository: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub worktree: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub base_ref: Option<String>,
    #[serde(default)]
    pub scopes: Vec<String>,
    #[serde(default)]
    pub durable_refs: Vec<String>,
    /// Assignment ids in the same run this assignment depends on. Advisory
    /// ordering: `worker start` refuses to launch until every dependency has
    /// reached a satisfied terminal state (see `dependency_state_satisfies`).
    /// Stored durably so a launched dependent's ordering survives compaction.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub depends_on: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub checkpoint: Option<RunCheckpoint>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub result_summary: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub blocker_summary: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub submit_recovery: Option<SubmitRecoveryRecord>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub worker_quarantine: Option<WorkerQuarantineRecord>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub account_handoff: Option<AccountHandoffReservationRecord>,
    pub created_at: String,
    pub updated_at: String,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct IdempotencyReceipt {
    pub principal_session_id: String,
    pub principal_incarnation: String,
    pub operation: String,
    pub request_digest: String,
    pub outcome: serde_json::Value,
    pub created_at_epoch: i64,
}

#[derive(Clone, Debug, Default, Deserialize, Serialize)]
#[serde(default, deny_unknown_fields)]
pub(crate) struct Registry {
    pub schema_version: String,
    pub runs: BTreeMap<String, RunRecord>,
    pub assignments: BTreeMap<String, AssignmentRecord>,
    pub receipts: BTreeMap<String, IdempotencyReceipt>,
}

#[derive(Clone, Debug, Serialize)]
pub(crate) struct WorkerCounts {
    pub assigned: usize,
    pub starting: usize,
    pub working: usize,
    pub blocked: usize,
    pub submitted: usize,
    pub accepted: usize,
    pub cleanup_pending: usize,
    pub orphaned: usize,
}

#[derive(Clone, Debug, Serialize)]
pub(crate) struct SessionOrchestrationProjection {
    pub schema_version: &'static str,
    pub run_id: String,
    pub role: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub assignment_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub primary_manager: Option<SessionRef>,
    pub relationship_revision: u64,
    pub run_state: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub assignment_state: Option<String>,
    pub objective_summary: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub collaborators: Vec<SessionRef>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub borrowed_by: Vec<SessionRef>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub relationship_state: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub worker_counts: Option<WorkerCounts>,
}

impl Registry {
    fn empty() -> Self {
        Self {
            schema_version: REGISTRY_SCHEMA.to_string(),
            ..Self::default()
        }
    }

    fn validate(&self) -> Result<(), CliError> {
        if self.schema_version != REGISTRY_SCHEMA {
            return Err(store_invalid("unsupported orchestration registry schema"));
        }
        if self.runs.len() > 1_024
            || self.assignments.len() > 16_384
            || self.receipts.len() > 32_768
        {
            return Err(store_invalid(
                "orchestration registry exceeds collection limits",
            ));
        }
        for (id, run) in &self.runs {
            if run.schema_version != RUN_SCHEMA || id != &run.run_id || run.revision == 0 {
                return Err(store_invalid("orchestration run identity is invalid"));
            }
            validate_slug("run id", id, 128)?;
            validate_state(
                &run.state,
                &["active", "orphaned", "recovery_needed", "closed"],
            )?;
            validate_summary("objective summary", &run.objective_summary)?;
            validate_digest(&run.objective_packet_digest)?;
            validate_session_ref(&run.controller)?;
        }
        for (id, assignment) in &self.assignments {
            if assignment.schema_version != ASSIGNMENT_SCHEMA
                || id != &assignment.assignment_id
                || assignment.revision == 0
                || !self.runs.contains_key(&assignment.run_id)
            {
                return Err(store_invalid(
                    "orchestration assignment identity is invalid",
                ));
            }
            validate_slug("assignment id", id, 128)?;
            validate_state(
                &assignment.state,
                &[
                    "assigned",
                    "starting",
                    "working",
                    "blocked",
                    "submitted",
                    "accepted",
                    "released",
                    "cancelled",
                ],
            )?;
            validate_summary("task summary", &assignment.task_summary)?;
            validate_digest(&assignment.private_packet_digest)?;
            validate_session_ref(&assignment.primary_manager)?;
            if let Some(worker) = &assignment.worker {
                validate_session_ref(worker)?;
            }
            if let Some(previous) = &assignment.previous_worker {
                validate_session_ref(previous)?;
                let Some(worker) = assignment.worker.as_ref() else {
                    return Err(store_invalid(
                        "orchestration previous worker identity is invalid",
                    ));
                };
                if previous.session_id != worker.session_id
                    || previous.session_created_at != worker.session_created_at
                    || previous.session_incarnation == worker.session_incarnation
                {
                    return Err(store_invalid(
                        "orchestration previous worker identity is invalid",
                    ));
                }
            }
            if let Some(recovery) = &assignment.submit_recovery {
                if recovery.schema_version != SUBMIT_RECOVERY_SCHEMA
                    || recovery.attempt_id.is_empty()
                    || recovery.attempt_id.len() > 128
                    || !matches!(recovery.origin.as_str(), "automatic" | "explicit")
                    || recovery.attempt_count != 1
                    || recovery.reserved_revision == 0
                    || recovery.session_incarnation.is_empty()
                    || recovery.session_incarnation.len() > 128
                {
                    return Err(store_invalid(
                        "orchestration submit recovery identity is invalid",
                    ));
                }
                match (&recovery.run_id, &recovery.controller) {
                    (Some(run_id), Some(controller)) => {
                        validate_slug("submit recovery run id", run_id, 128)?;
                        validate_session_ref(controller)?;
                    }
                    (None, None) => {}
                    _ => {
                        return Err(store_invalid(
                            "orchestration submit recovery controller binding is incomplete",
                        ));
                    }
                }
                validate_state(
                    &recovery.state,
                    &[
                        "attempting",
                        "sent",
                        "failed",
                        "checkpoint_confirmed",
                        "reconciled",
                    ],
                )?;
                validate_summary("submit recovery result", &recovery.result)?;
            }
            if let Some(quarantine) = &assignment.worker_quarantine
                && (validate_worker_quarantine(quarantine).is_err()
                    || assignment.worker.as_ref() != Some(&quarantine.worker)
                    || assignment.submit_recovery.as_ref().is_none_or(|recovery| {
                        recovery.state != "reconciled"
                            || recovery.session_incarnation != quarantine.worker.session_incarnation
                    }))
            {
                return Err(store_invalid(
                    "orchestration worker quarantine identity is invalid",
                ));
            }
            if let Some(reservation) = &assignment.account_handoff
                && (!matches!(
                    reservation.schema_version.as_str(),
                    ACCOUNT_HANDOFF_RESERVATION_SCHEMA
                        | LEGACY_ACCOUNT_HANDOFF_RESERVATION_V2_SCHEMA
                        | LEGACY_ACCOUNT_HANDOFF_RESERVATION_SCHEMA
                ) || reservation.request_digest.len() != 64
                    || !reservation
                        .request_digest
                        .bytes()
                        .all(|byte| byte.is_ascii_hexdigit())
                    || match (
                        reservation.schema_version.as_str(),
                        reservation.reservation_id.as_deref(),
                        reservation.account_intent_id.as_deref(),
                    ) {
                        (
                            ACCOUNT_HANDOFF_RESERVATION_SCHEMA,
                            Some(reservation_id),
                            Some(intent_id),
                        ) => {
                            reservation_id.is_empty()
                                || reservation_id.len() > 128
                                || !reservation_id
                                    .bytes()
                                    .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-')
                                || intent_id.is_empty()
                                || intent_id.len() > 128
                                || !intent_id
                                    .bytes()
                                    .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-')
                        }
                        (LEGACY_ACCOUNT_HANDOFF_RESERVATION_V2_SCHEMA, None, Some(intent_id)) => {
                            intent_id.is_empty()
                                || intent_id.len() > 128
                                || !intent_id
                                    .bytes()
                                    .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-')
                        }
                        (LEGACY_ACCOUNT_HANDOFF_RESERVATION_SCHEMA, None, None) => false,
                        _ => true,
                    }
                    || reservation.run_id != assignment.run_id
                    || reservation.controller != assignment.primary_manager
                    || assignment.worker.as_ref() != Some(&reservation.worker)
                    || match reservation.schema_version.as_str() {
                        ACCOUNT_HANDOFF_RESERVATION_SCHEMA => reservation
                            .reserved_revision
                            .checked_add(1)
                            .is_none_or(|revision| revision != assignment.revision),
                        _ => reservation.reserved_revision != assignment.revision,
                    }
                    || reservation.account.is_empty()
                    || reservation.account.len() > 128
                    || reservation.created_at.is_empty()
                    || reservation.updated_at.is_empty())
            {
                return Err(store_invalid(
                    "orchestration account handoff reservation identity is invalid",
                ));
            }
            for collaborator in &assignment.collaborators {
                validate_session_ref(collaborator)?;
            }
            for relationship in &assignment.borrowed_by {
                validate_session_ref(&relationship.session)?;
            }
            // Dependency edges are bounds/format-checked only. Referential
            // existence is intentionally NOT a registry invariant: a dependency
            // may be released and deleted after a dependent launches, and that
            // must not brick registry reads. Existence/satisfaction is enforced
            // at `worker start` gate time against live state instead.
            if assignment.depends_on.len() > 64 {
                return Err(store_invalid(
                    "orchestration assignment exceeds dependency limit",
                ));
            }
            for dependency in &assignment.depends_on {
                validate_slug("assignment dependency id", dependency, 128)?;
                if dependency == &assignment.assignment_id {
                    return Err(store_invalid(
                        "orchestration assignment cannot depend on itself",
                    ));
                }
            }
        }
        Ok(())
    }
}

pub(crate) fn session_projection(
    context: &CliContext,
    record: &SessionRecord,
) -> Result<Option<SessionOrchestrationProjection>, CliError> {
    let registry = load_registry_readonly(context)?;
    let Some(incarnation) = record
        .runtime
        .as_ref()
        .map(|runtime| runtime.launch_id.as_str())
        .filter(|value| !value.is_empty())
    else {
        return Ok(None);
    };
    if let Some(run) = registry
        .runs
        .values()
        .find(|run| session_ref_matches(&run.controller, record, incarnation))
    {
        return Ok(Some(SessionOrchestrationProjection {
            schema_version: SESSION_PROJECTION_SCHEMA,
            run_id: run.run_id.clone(),
            role: "main".to_string(),
            assignment_id: None,
            primary_manager: None,
            relationship_revision: run.revision,
            run_state: run.state.clone(),
            assignment_state: None,
            objective_summary: run.objective_summary.clone(),
            collaborators: Vec::new(),
            borrowed_by: Vec::new(),
            relationship_state: (run.state != "active").then(|| run.state.clone()),
            worker_counts: Some(worker_counts(context, &registry, run)),
        }));
    }
    if let Some(assignment) = registry.assignments.values().find(|assignment| {
        assignment.worker.as_ref().is_some_and(|worker| {
            worker.session_id == record.id && worker.session_created_at == record.created_at
        })
    }) {
        let Some(run) = registry.runs.get(&assignment.run_id) else {
            return Ok(None);
        };
        let rebind_required = assignment
            .worker
            .as_ref()
            .is_some_and(|worker| worker.session_incarnation != incarnation);
        let now = crate::coordination::now_epoch();
        let borrowed_by = assignment
            .borrowed_by
            .iter()
            .filter(|relationship| relationship.expires_at_epoch > now)
            .map(|relationship| relationship.session.clone())
            .collect::<Vec<_>>();
        let relationship_state = if rebind_required {
            Some("rebind_required".to_string())
        } else if !controller_is_current(context, run) {
            Some("orphaned".to_string())
        } else if !borrowed_by.is_empty() {
            Some("borrowed".to_string())
        } else if !assignment.collaborators.is_empty() {
            Some("cross_managed".to_string())
        } else {
            None
        };
        return Ok(Some(SessionOrchestrationProjection {
            schema_version: SESSION_PROJECTION_SCHEMA,
            run_id: run.run_id.clone(),
            role: "worker".to_string(),
            assignment_id: Some(assignment.assignment_id.clone()),
            primary_manager: Some(assignment.primary_manager.clone()),
            relationship_revision: assignment.revision,
            run_state: run.state.clone(),
            assignment_state: Some(assignment.state.clone()),
            objective_summary: run.objective_summary.clone(),
            collaborators: assignment.collaborators.clone(),
            borrowed_by,
            relationship_state,
            worker_counts: None,
        }));
    }
    Ok(None)
}

fn worker_counts(context: &CliContext, registry: &Registry, run: &RunRecord) -> WorkerCounts {
    let mut counts = WorkerCounts {
        assigned: 0,
        starting: 0,
        working: 0,
        blocked: 0,
        submitted: 0,
        accepted: 0,
        cleanup_pending: 0,
        orphaned: 0,
    };
    for assignment in registry
        .assignments
        .values()
        .filter(|item| item.run_id == run.run_id)
    {
        match assignment.state.as_str() {
            "assigned" => counts.assigned += 1,
            "starting" => counts.starting += 1,
            "working" => counts.working += 1,
            "blocked" => counts.blocked += 1,
            "submitted" => counts.submitted += 1,
            "accepted" => counts.accepted += 1,
            "released" | "cancelled" => {
                counts.cleanup_pending += usize::from(
                    assignment
                        .worker
                        .as_ref()
                        .is_some_and(|worker| session_ref_is_live(context, worker)),
                )
            }
            _ => {}
        }
        if assignment.worker.is_some() && !controller_is_current(context, run) {
            counts.orphaned += 1;
        }
    }
    counts
}

pub(crate) fn controller_is_current(context: &CliContext, run: &RunRecord) -> bool {
    session_ref_is_live(context, &run.controller)
}

pub(crate) fn session_ref_is_live(context: &CliContext, reference: &SessionRef) -> bool {
    crate::load_session_record(context, &reference.session_id)
        .ok()
        .and_then(|record| {
            record.runtime.as_ref().map(|runtime| {
                runtime.launch_id == reference.session_incarnation
                    && record.created_at == reference.session_created_at
            })
        })
        .unwrap_or(false)
}

pub(crate) fn session_ref_matches(
    reference: &SessionRef,
    record: &SessionRecord,
    incarnation: &str,
) -> bool {
    reference.session_id == record.id
        && reference.session_incarnation == incarnation
        && reference.session_created_at == record.created_at
}

pub(crate) fn ensure_session_not_quarantined(
    context: &CliContext,
    record: &SessionRecord,
) -> Result<(), CliError> {
    if let Some(marker) = validated_session_group_cleanup_fence(context, record)? {
        return Err(CliError::data(
            "worker-group-cleanup-fenced",
            "worker execution authority is fenced by Main Agent group cleanup",
            Some(json!({
                "run_id": marker.run_id,
                "main_session_id": marker.main.session_id,
            })),
        ));
    }
    let Some(marker) = validated_session_authority_quarantine(context, record)? else {
        return Ok(());
    };
    Err(CliError::data(
        "worker-quarantined",
        "worker execution authority is quarantined after stopped-runtime recovery reconciliation",
        Some(json!({
            "assignment_id": marker.assignment_id,
            "current_revision": marker.assignment_revision
        })),
    ))
}

pub(crate) fn session_authority_is_quarantined(
    context: &CliContext,
    record: &SessionRecord,
) -> Result<bool, CliError> {
    if validated_session_group_cleanup_fence(context, record)?.is_some() {
        return Ok(true);
    }
    validated_session_authority_quarantine(context, record).map(|marker| marker.is_some())
}

fn validated_session_group_cleanup_fence(
    context: &CliContext,
    record: &SessionRecord,
) -> Result<Option<SessionGroupCleanupFence>, CliError> {
    let Some(marker) = read_session_group_cleanup_fence(context, &record.id)? else {
        return Ok(None);
    };
    validate_session_group_cleanup_fence(&marker)?;
    let incarnation = record
        .runtime
        .as_ref()
        .map(|runtime| runtime.launch_id.as_str())
        .unwrap_or_default();
    if !session_ref_matches(&marker.worker, record, incarnation) {
        return Err(store_invalid(
            "session group cleanup fence identity is invalid",
        ));
    }
    Ok(Some(marker))
}

pub(crate) fn persist_session_group_cleanup_fence(
    context: &CliContext,
    worker: &SessionRef,
    main: &SessionRef,
    run_id: &str,
    plan_digest: &str,
) -> Result<SessionGroupCleanupFence, CliError> {
    let marker = SessionGroupCleanupFence {
        schema_version: SESSION_GROUP_CLEANUP_FENCE_SCHEMA.to_string(),
        worker: worker.clone(),
        main: main.clone(),
        run_id: run_id.to_string(),
        plan_digest: plan_digest.to_string(),
        created_at: crate::coordination::timestamp(crate::coordination::now_epoch()),
    };
    validate_session_group_cleanup_fence(&marker)?;
    if let Some(existing) = read_session_group_cleanup_fence(context, &worker.session_id)? {
        validate_session_group_cleanup_fence(&existing)?;
        let retry_matches = existing.schema_version == marker.schema_version
            && existing.worker == marker.worker
            && existing.main == marker.main
            && existing.run_id == marker.run_id
            && existing.plan_digest == marker.plan_digest;
        if !retry_matches {
            return Err(CliError::data(
                "worker-group-cleanup-fence-conflict",
                "worker execution authority has a different persistent group cleanup fence",
                None,
            ));
        }
        return Ok(existing);
    }
    let bytes = serde_json::to_vec_pretty(&marker)
        .map_err(|_| store_invalid("session group cleanup fence is invalid"))?;
    if bytes.len() as u64 > MAX_SESSION_AUTHORITY_QUARANTINE_BYTES {
        return Err(store_invalid(
            "session group cleanup fence exceeds byte limit",
        ));
    }
    let path =
        crate::session_dir(context, &worker.session_id).join(SESSION_GROUP_CLEANUP_FENCE_FILE);
    write_atomic(&path, &bytes, SECRET_FILE_MODE).map_err(|_| store_unavailable())?;
    Ok(marker)
}

fn read_session_group_cleanup_fence(
    context: &CliContext,
    session_id: &str,
) -> Result<Option<SessionGroupCleanupFence>, CliError> {
    let path = crate::session_dir(context, session_id).join(SESSION_GROUP_CLEANUP_FENCE_FILE);
    let Some(snapshot) = read_private_bounded_file_with_limit(
        &path,
        MAX_SESSION_AUTHORITY_QUARANTINE_BYTES,
        "session group cleanup fence permissions are unsafe",
        "session group cleanup fence exceeds byte limit",
        "session group cleanup fence changed while it was being read",
    )?
    else {
        return Ok(None);
    };
    serde_json::from_slice(&snapshot.bytes)
        .map(Some)
        .map_err(|_| store_invalid("session group cleanup fence is invalid"))
}

fn validate_session_group_cleanup_fence(marker: &SessionGroupCleanupFence) -> Result<(), CliError> {
    if marker.schema_version != SESSION_GROUP_CLEANUP_FENCE_SCHEMA {
        return Err(store_invalid(
            "session group cleanup fence schema is invalid",
        ));
    }
    validate_session_ref(&marker.worker)?;
    validate_session_ref(&marker.main)?;
    validate_slug("group cleanup run id", &marker.run_id, 128)?;
    validate_digest(&marker.plan_digest)?;
    if marker.created_at.trim().is_empty() || marker.created_at.len() > 64 {
        return Err(store_invalid(
            "session group cleanup fence timestamp is invalid",
        ));
    }
    Ok(())
}

fn validated_session_authority_quarantine(
    context: &CliContext,
    record: &SessionRecord,
) -> Result<Option<SessionAuthorityQuarantine>, CliError> {
    let Some(marker) = read_session_authority_quarantine(context, &record.id)? else {
        return Ok(None);
    };
    validate_session_authority_quarantine(&marker)?;
    if marker.quarantine.worker.session_id != record.id
        || marker.quarantine.worker.session_created_at != record.created_at
    {
        return Err(store_invalid(
            "session authority quarantine identity is invalid",
        ));
    }
    Ok(Some(marker))
}

pub(crate) fn persist_session_authority_quarantine(
    context: &CliContext,
    assignment_id: &str,
    assignment_revision: u64,
    quarantine: &WorkerQuarantineRecord,
) -> Result<WorkerQuarantineRecord, CliError> {
    let marker = SessionAuthorityQuarantine {
        schema_version: SESSION_AUTHORITY_QUARANTINE_SCHEMA.to_string(),
        assignment_id: assignment_id.to_string(),
        assignment_revision,
        quarantine: quarantine.clone(),
    };
    validate_session_authority_quarantine(&marker)?;
    if let Some(existing) =
        read_session_authority_quarantine(context, &quarantine.worker.session_id)?
    {
        validate_session_authority_quarantine(&existing)?;
        let retry_matches = existing.schema_version == marker.schema_version
            && existing.assignment_id == marker.assignment_id
            && existing.assignment_revision == marker.assignment_revision
            && existing.quarantine.schema_version == marker.quarantine.schema_version
            && existing.quarantine.worker == marker.quarantine.worker
            && existing.quarantine.reason == marker.quarantine.reason
            && existing.quarantine.runtime_identity_digest
                == marker.quarantine.runtime_identity_digest;
        if !retry_matches {
            return Err(CliError::data(
                "worker-quarantine-conflict",
                "worker execution authority has a different persistent quarantine",
                None,
            ));
        }
        return Ok(existing.quarantine);
    }
    let bytes = serde_json::to_vec_pretty(&marker)
        .map_err(|_| store_invalid("session authority quarantine is invalid"))?;
    if bytes.len() as u64 > MAX_SESSION_AUTHORITY_QUARANTINE_BYTES {
        return Err(store_invalid(
            "session authority quarantine exceeds byte limit",
        ));
    }
    let path = crate::session_dir(context, &quarantine.worker.session_id)
        .join(SESSION_AUTHORITY_QUARANTINE_FILE);
    write_atomic(&path, &bytes, SECRET_FILE_MODE).map_err(|_| store_unavailable())?;
    Ok(quarantine.clone())
}

fn read_session_authority_quarantine(
    context: &CliContext,
    session_id: &str,
) -> Result<Option<SessionAuthorityQuarantine>, CliError> {
    let path = crate::session_dir(context, session_id).join(SESSION_AUTHORITY_QUARANTINE_FILE);
    let Some(snapshot) = read_private_bounded_file_with_limit(
        &path,
        MAX_SESSION_AUTHORITY_QUARANTINE_BYTES,
        "session authority quarantine permissions are unsafe",
        "session authority quarantine exceeds byte limit",
        "session authority quarantine changed while it was being read",
    )?
    else {
        return Ok(None);
    };
    serde_json::from_slice(&snapshot.bytes)
        .map(Some)
        .map_err(|_| store_invalid("session authority quarantine is invalid"))
}

fn validate_session_authority_quarantine(
    marker: &SessionAuthorityQuarantine,
) -> Result<(), CliError> {
    if marker.schema_version != SESSION_AUTHORITY_QUARANTINE_SCHEMA {
        return Err(store_invalid(
            "session authority quarantine schema is invalid",
        ));
    }
    validate_slug("quarantine assignment id", &marker.assignment_id, 128)?;
    validate_worker_quarantine(&marker.quarantine)
}

fn validate_worker_quarantine(quarantine: &WorkerQuarantineRecord) -> Result<(), CliError> {
    if quarantine.schema_version != WORKER_QUARANTINE_SCHEMA {
        return Err(store_invalid(
            "orchestration worker quarantine identity is invalid",
        ));
    }
    validate_session_ref(&quarantine.worker)?;
    validate_summary("worker quarantine reason", &quarantine.reason)?;
    validate_digest(&quarantine.runtime_identity_digest)?;
    if quarantine.created_at.trim().is_empty() || quarantine.created_at.len() > 64 {
        return Err(store_invalid(
            "orchestration worker quarantine timestamp is invalid",
        ));
    }
    Ok(())
}

pub(crate) fn load_registry_readonly(context: &CliContext) -> Result<Registry, CliError> {
    let path = orchestration_root(context).join(REGISTRY_FILE);
    let Some(bytes) = read_registry_bytes(&path)? else {
        return Ok(Registry::empty());
    };
    decode_registry_bytes(&bytes).map(|(registry, _)| registry)
}

fn read_registry_bytes(path: &Path) -> Result<Option<Vec<u8>>, CliError> {
    read_private_bounded_file(path, "orchestration registry exceeds byte limit")
        .map(|file| file.map(|file| file.bytes))
}

struct PrivateBoundedFile {
    file: File,
    bytes: Vec<u8>,
    snapshot: fs::Metadata,
}

fn read_private_bounded_file(
    path: &Path,
    oversized_message: &'static str,
) -> Result<Option<PrivateBoundedFile>, CliError> {
    read_private_bounded_file_with_limit(
        path,
        MAX_REGISTRY_BYTES,
        "orchestration registry permissions are unsafe",
        oversized_message,
        "orchestration registry changed while it was being read",
    )
}

fn read_private_bounded_file_with_limit(
    path: &Path,
    max_bytes: u64,
    unsafe_message: &'static str,
    oversized_message: &'static str,
    changed_message: &'static str,
) -> Result<Option<PrivateBoundedFile>, CliError> {
    let mut file = match OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC | libc::O_NONBLOCK)
        .open(path)
    {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) if error.raw_os_error() == Some(libc::ELOOP) => {
            return Err(store_invalid(unsafe_message));
        }
        Err(_) => return Err(store_unavailable()),
    };
    let before = file.metadata().map_err(|_| store_unavailable())?;
    validate_private_file(&before)?;
    if before.len() > max_bytes {
        return Err(store_invalid(oversized_message));
    }
    #[cfg(test)]
    replace_private_read_path_for_test(path);
    let mut bytes = Vec::with_capacity(before.len() as usize);
    (&mut file)
        .take(max_bytes + 1)
        .read_to_end(&mut bytes)
        .map_err(|_| store_unavailable())?;
    if bytes.len() as u64 > max_bytes {
        return Err(store_invalid(oversized_message));
    }
    let after = file.metadata().map_err(|_| store_unavailable())?;
    if !same_private_file_snapshot(&before, &after) || bytes.len() as u64 != after.len() {
        return Err(store_invalid(changed_message));
    }
    let path_metadata = fs::symlink_metadata(path).map_err(|_| store_invalid(changed_message))?;
    validate_private_file(&path_metadata)?;
    if after.dev() != path_metadata.dev() || after.ino() != path_metadata.ino() {
        return Err(store_invalid(changed_message));
    }
    Ok(Some(PrivateBoundedFile {
        file,
        bytes,
        snapshot: after,
    }))
}

fn same_private_file_snapshot(before: &fs::Metadata, after: &fs::Metadata) -> bool {
    before.dev() == after.dev()
        && before.ino() == after.ino()
        && before.len() == after.len()
        && before.mtime() == after.mtime()
        && before.mtime_nsec() == after.mtime_nsec()
        && before.ctime() == after.ctime()
        && before.ctime_nsec() == after.ctime_nsec()
}

fn decode_registry_bytes(bytes: &[u8]) -> Result<(Registry, String), CliError> {
    let mut registry: Registry = serde_json::from_slice(bytes)
        .map_err(|_| store_invalid("orchestration registry is invalid"))?;
    let source_schema = registry.schema_version.clone();
    if matches!(
        registry.schema_version.as_str(),
        LEGACY_REGISTRY_V2_SCHEMA | LEGACY_REGISTRY_V1_SCHEMA
    ) {
        registry.schema_version = REGISTRY_SCHEMA.to_string();
    }
    for assignment in registry.assignments.values_mut() {
        if matches!(
            assignment.schema_version.as_str(),
            LEGACY_ASSIGNMENT_V2_SCHEMA | LEGACY_ASSIGNMENT_V1_SCHEMA
        ) {
            assignment.schema_version = ASSIGNMENT_SCHEMA.to_string();
        }
    }
    registry.validate()?;
    Ok((registry, source_schema))
}

pub(crate) struct LockedRegistry {
    _lock: File,
    path: PathBuf,
    rollback_path: PathBuf,
    legacy_source: Option<Vec<u8>>,
    pub registry: Registry,
}

impl LockedRegistry {
    pub fn save(&mut self) -> Result<(), CliError> {
        self.save_with_rollback_durability(sync_rollback_snapshot)
    }

    fn save_with_rollback_durability(
        &mut self,
        ensure_rollback_durable: impl FnOnce(&PrivateBoundedFile, &Path) -> Result<(), CliError>,
    ) -> Result<(), CliError> {
        self.registry.schema_version = REGISTRY_SCHEMA.to_string();
        self.registry.validate()?;
        let bytes = serde_json::to_vec_pretty(&self.registry)
            .map_err(|_| store_invalid("orchestration registry is invalid"))?;
        #[cfg(test)]
        REGISTRY_SAVE_BYTES_FOR_TEST
            .fetch_add(bytes.len() as u64, std::sync::atomic::Ordering::AcqRel);
        if bytes.len() as u64 > MAX_REGISTRY_BYTES {
            return Err(store_invalid("orchestration registry exceeds byte limit"));
        }
        if let Some(legacy_source) = self.legacy_source.as_ref() {
            let rollback = match read_private_bounded_file(
                &self.rollback_path,
                "orchestration rollback snapshot exceeds byte limit",
            )? {
                Some(rollback) => {
                    if rollback.bytes != *legacy_source {
                        return Err(store_invalid(
                            "orchestration rollback snapshot does not match the migration source",
                        ));
                    }
                    rollback
                }
                None => {
                    write_atomic(&self.rollback_path, legacy_source, SECRET_FILE_MODE)
                        .map_err(|_| store_unavailable())?;
                    let rollback = read_private_bounded_file(
                        &self.rollback_path,
                        "orchestration rollback snapshot exceeds byte limit",
                    )?
                    .ok_or_else(store_unavailable)?;
                    if rollback.bytes != *legacy_source {
                        return Err(store_invalid(
                            "orchestration rollback snapshot does not match the migration source",
                        ));
                    }
                    rollback
                }
            };
            ensure_rollback_durable(&rollback, &self.rollback_path)?;
        }
        write_atomic(&self.path, &bytes, SECRET_FILE_MODE).map_err(|_| store_unavailable())?;
        self.legacy_source = None;
        Ok(())
    }
}

fn sync_rollback_snapshot(snapshot: &PrivateBoundedFile, path: &Path) -> Result<(), CliError> {
    verify_private_descriptor_path(snapshot, path)?;
    snapshot.file.sync_all().map_err(|_| store_unavailable())?;
    let parent = path.parent().ok_or_else(store_unavailable)?;
    let directory = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC)
        .open(parent)
        .map_err(|_| store_unavailable())?;
    let directory_metadata = directory.metadata().map_err(|_| store_unavailable())?;
    if !directory_metadata.is_dir()
        || directory_metadata.uid() != unsafe { libc::geteuid() }
        || directory_metadata.mode() & 0o077 != 0
    {
        return Err(store_invalid("orchestration store root is unsafe"));
    }
    directory
        .sync_all()
        .map_err(|_| store_unavailable())
        .and_then(|()| verify_private_descriptor_path(snapshot, path))
}

fn verify_private_descriptor_path(
    snapshot: &PrivateBoundedFile,
    path: &Path,
) -> Result<(), CliError> {
    let descriptor_metadata = snapshot.file.metadata().map_err(|_| store_unavailable())?;
    if !same_private_file_snapshot(&snapshot.snapshot, &descriptor_metadata) {
        return Err(store_invalid(
            "orchestration rollback snapshot changed before it was durable",
        ));
    }
    let path_metadata = fs::symlink_metadata(path).map_err(|_| store_unavailable())?;
    validate_private_file(&path_metadata)?;
    if descriptor_metadata.dev() != path_metadata.dev()
        || descriptor_metadata.ino() != path_metadata.ino()
    {
        return Err(store_invalid(
            "orchestration rollback snapshot path changed before it was durable",
        ));
    }
    Ok(())
}

pub(crate) fn lock_registry(context: &CliContext) -> Result<LockedRegistry, CliError> {
    let root = ensure_orchestration_root(context)?;
    let path = root.join(REGISTRY_FILE);
    let lock_path = root.join(REGISTRY_LOCK);
    let lock = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .mode(SECRET_FILE_MODE)
        .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC)
        .open(&lock_path)
        .map_err(|_| store_unavailable())?;
    let started = Instant::now();
    loop {
        // SAFETY: the descriptor remains open for the duration of the lock guard.
        let result = unsafe { libc::flock(lock.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) };
        if result == 0 {
            break;
        }
        if started.elapsed() >= LOCK_TIMEOUT {
            return Err(CliError::unavailable(
                "orchestration-store-busy",
                "orchestration store is busy; retry with the same idempotency key",
                None,
            ));
        }
        thread::sleep(Duration::from_millis(20));
    }
    let source = read_registry_bytes(&path)?;
    let (registry, source_schema) = source.as_deref().map_or_else(
        || Ok((Registry::empty(), REGISTRY_SCHEMA.to_string())),
        decode_registry_bytes,
    )?;
    let (legacy_source, rollback_path) = match source_schema.as_str() {
        LEGACY_REGISTRY_V2_SCHEMA => (source, root.join(REGISTRY_V2_ROLLBACK_FILE)),
        LEGACY_REGISTRY_V1_SCHEMA => (source, root.join(REGISTRY_V1_ROLLBACK_FILE)),
        _ => (None, root.join(REGISTRY_V2_ROLLBACK_FILE)),
    };
    Ok(LockedRegistry {
        _lock: lock,
        rollback_path,
        path,
        legacy_source,
        registry,
    })
}

pub(crate) fn packet_path(context: &CliContext, digest: &str) -> Result<PathBuf, CliError> {
    validate_digest(digest)?;
    let root = ensure_orchestration_root(context)?.join(PACKETS_DIR);
    ensure_private_directory(&root)?;
    Ok(root.join(digest.trim_start_matches("sha256:")))
}

fn group_cleanup_progress_path(context: &CliContext, key: &str) -> Result<PathBuf, CliError> {
    validate_slug("group cleanup progress key", key, 128)?;
    let root = ensure_orchestration_root(context)?.join(GROUP_CLEANUP_PROGRESS_DIR);
    ensure_private_directory(&root)?;
    Ok(root.join(key))
}

pub(crate) fn store_group_cleanup_progress(
    context: &CliContext,
    key: &str,
    bytes: &[u8],
) -> Result<(), CliError> {
    if bytes.len() as u64 > MAX_REGISTRY_BYTES {
        return Err(store_invalid("group cleanup progress exceeds byte limit"));
    }
    let path = group_cleanup_progress_path(context, key)?;
    let _lock = lock_group_cleanup_progress(context)?;
    if prune_group_cleanup_progress_for_write(context, key, bytes)? {
        return Ok(());
    }
    write_atomic(&path, bytes, SECRET_FILE_MODE).map_err(|_| store_unavailable())
}

fn lock_group_cleanup_progress(context: &CliContext) -> Result<File, CliError> {
    let orchestration_root = ensure_orchestration_root(context)?;
    let lock = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .mode(SECRET_FILE_MODE)
        .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC)
        .open(orchestration_root.join(GROUP_CLEANUP_PROGRESS_LOCK))
        .map_err(|_| store_unavailable())?;
    let started = Instant::now();
    loop {
        // SAFETY: the descriptor remains open until pruning and replacement
        // complete, serializing the aggregate count/byte admission check.
        let result = unsafe { libc::flock(lock.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) };
        if result == 0 {
            break;
        }
        if started.elapsed() >= LOCK_TIMEOUT {
            return Err(CliError::unavailable(
                "orchestration-store-busy",
                "orchestration store is busy; retry with the same idempotency key",
                None,
            ));
        }
        thread::sleep(Duration::from_millis(20));
    }
    Ok(lock)
}

#[derive(Debug)]
struct GroupCleanupProgressEntry {
    path: PathBuf,
    key: String,
    snapshot: fs::Metadata,
}

fn prune_group_cleanup_progress_for_write(
    context: &CliContext,
    current_key: &str,
    incoming: &[u8],
) -> Result<bool, CliError> {
    let incoming_len = incoming.len() as u64;
    let root = ensure_orchestration_root(context)?.join(GROUP_CLEANUP_PROGRESS_DIR);
    ensure_private_directory(&root)?;
    let recycle_path = group_cleanup_progress_recycle_path(context)?;
    recover_group_cleanup_progress_recycle(context, &recycle_path)?;
    let mut entries = Vec::new();
    let mut scanned_bytes = 0_u64;
    for entry in fs::read_dir(&root).map_err(|_| store_unavailable())? {
        #[cfg(test)]
        count_group_cleanup_progress_visit_for_test(&root);
        let entry = entry.map_err(|_| store_unavailable())?;
        let key = entry
            .file_name()
            .into_string()
            .map_err(|_| store_invalid("group cleanup progress key is invalid"))?;
        validate_slug("group cleanup progress key", &key, 128)?;
        let path = entry.path();
        let metadata = fs::symlink_metadata(&path).map_err(|_| store_unavailable())?;
        validate_private_file(&metadata)?;
        if metadata.len() > MAX_REGISTRY_BYTES {
            return Err(store_invalid("group cleanup progress exceeds byte limit"));
        }
        let len = metadata.len();
        scanned_bytes = scanned_bytes
            .checked_add(len)
            .ok_or_else(|| store_invalid("group cleanup progress aggregate is invalid"))?;
        entries.push(GroupCleanupProgressEntry {
            path,
            key,
            snapshot: metadata,
        });
        if entries.len() > MAX_GROUP_CLEANUP_PROGRESS_FILES
            || scanned_bytes > MAX_GROUP_CLEANUP_PROGRESS_BYTES
        {
            return Err(group_cleanup_progress_capacity());
        }
    }
    #[cfg(test)]
    pause_group_cleanup_progress_after_scan_for_test(&root);

    let replaced_len = entries
        .iter()
        .find(|entry| entry.key == current_key)
        .map_or(0, |entry| entry.snapshot.len());
    let mut projected_files =
        entries.len() + usize::from(!entries.iter().any(|entry| entry.key == current_key));
    let mut projected_bytes = entries
        .iter()
        .try_fold(0_u64, |total, entry| {
            total.checked_add(entry.snapshot.len())
        })
        .and_then(|total| total.checked_sub(replaced_len))
        .and_then(|total| total.checked_add(incoming_len))
        .ok_or_else(|| store_invalid("group cleanup progress aggregate is invalid"))?;
    if projected_files <= MAX_GROUP_CLEANUP_PROGRESS_FILES
        && projected_bytes <= MAX_GROUP_CLEANUP_PROGRESS_BYTES
    {
        return Ok(false);
    }

    let current_path = root.join(current_key);
    let current_exists = entries.iter().any(|entry| entry.key == current_key);
    let mut candidates = entries
        .into_iter()
        .filter(|entry| entry.key != current_key)
        .collect::<Vec<_>>();
    candidates.sort_by_key(|entry| entry.snapshot.mtime());
    let retired_len = group_cleanup_progress_retired_receipt().len() as u64;
    let needs_recycle = projected_files > MAX_GROUP_CLEANUP_PROGRESS_FILES && !current_exists;
    let mut preflight_bytes = projected_bytes;
    let mut planned_compactions = Vec::new();
    let mut planned_recycle = None;
    for entry in candidates {
        let stale_snapshot = match group_cleanup_progress_principal_liveness(context, &entry) {
            GroupCleanupProgressLiveness::Stale(snapshot) => snapshot,
            GroupCleanupProgressLiveness::Live | GroupCleanupProgressLiveness::Unverifiable => {
                continue;
            }
        };
        if needs_recycle
            && preflight_bytes.saturating_sub(entry.snapshot.len())
                <= MAX_GROUP_CLEANUP_PROGRESS_BYTES
        {
            planned_recycle = Some((entry, stale_snapshot));
            break;
        }
        if preflight_bytes > MAX_GROUP_CLEANUP_PROGRESS_BYTES && entry.snapshot.len() > retired_len
        {
            preflight_bytes = preflight_bytes
                .saturating_sub(entry.snapshot.len())
                .saturating_add(retired_len);
            planned_compactions.push((entry, stale_snapshot));
            if !needs_recycle && preflight_bytes <= MAX_GROUP_CLEANUP_PROGRESS_BYTES {
                break;
            }
        }
    }
    if (needs_recycle && planned_recycle.is_none())
        || (!needs_recycle && preflight_bytes > MAX_GROUP_CLEANUP_PROGRESS_BYTES)
    {
        return Err(group_cleanup_progress_capacity());
    }

    for (entry, stale_snapshot) in planned_compactions {
        let Some(compacted_len) =
            compact_stale_group_cleanup_progress(context, &stale_snapshot, &entry.path)?
        else {
            return Err(group_cleanup_progress_capacity());
        };
        projected_bytes = projected_bytes
            .saturating_sub(entry.snapshot.len())
            .saturating_add(compacted_len);
    }
    if let Some((entry, stale_snapshot)) = planned_recycle {
        if projected_bytes.saturating_sub(entry.snapshot.len()) > MAX_GROUP_CLEANUP_PROGRESS_BYTES {
            return Err(group_cleanup_progress_capacity());
        }
        if recycle_stale_group_cleanup_progress_into_current(
            context,
            &stale_snapshot,
            &entry.path,
            &current_path,
            incoming,
        )? {
            projected_files = projected_files.saturating_sub(1);
            projected_bytes = projected_bytes.saturating_sub(entry.snapshot.len());
            if projected_files <= MAX_GROUP_CLEANUP_PROGRESS_FILES
                && projected_bytes <= MAX_GROUP_CLEANUP_PROGRESS_BYTES
            {
                return Ok(true);
            }
        }
        return Err(group_cleanup_progress_capacity());
    }
    if projected_bytes <= MAX_GROUP_CLEANUP_PROGRESS_BYTES {
        Ok(false)
    } else {
        Err(group_cleanup_progress_capacity())
    }
}

enum GroupCleanupProgressLiveness {
    Live,
    Stale(Box<PrivateBoundedFile>),
    Unverifiable,
}

fn group_cleanup_progress_principal_liveness(
    context: &CliContext,
    entry: &GroupCleanupProgressEntry,
) -> GroupCleanupProgressLiveness {
    #[cfg(test)]
    GROUP_CLEANUP_PROGRESS_BODY_READS_FOR_TEST.set(
        GROUP_CLEANUP_PROGRESS_BODY_READS_FOR_TEST
            .get()
            .saturating_add(1),
    );
    #[cfg(test)]
    if group_cleanup_progress_read_fails_for_test(&entry.path) {
        return GroupCleanupProgressLiveness::Unverifiable;
    }
    let Ok(Some(snapshot)) = read_private_bounded_file_with_limit(
        &entry.path,
        MAX_REGISTRY_BYTES,
        "group cleanup progress permissions are unsafe",
        "group cleanup progress exceeds byte limit",
        "group cleanup progress changed while it was being read",
    ) else {
        return GroupCleanupProgressLiveness::Unverifiable;
    };
    if !same_private_file_snapshot(&entry.snapshot, &snapshot.snapshot) {
        return GroupCleanupProgressLiveness::Unverifiable;
    }
    let Ok(receipt) = decode_group_cleanup_progress_receipt(&snapshot.bytes) else {
        return GroupCleanupProgressLiveness::Unverifiable;
    };
    let session_id = receipt.principal_session_id.as_str();
    let incarnation = receipt.principal_incarnation.as_str();
    let canonical_session_id = receipt.outcome["_resume"]["plan"]["main"]["session_id"]
        .as_str()
        .unwrap_or(session_id);
    if crate::validate_id(canonical_session_id).is_err() {
        return GroupCleanupProgressLiveness::Unverifiable;
    }
    if receipt.outcome["_resume"]["pending_registry_fences"]
        .as_array()
        .is_some_and(|fences| {
            fences.iter().any(|fence| {
                fence["session_id"].as_str() == Some(canonical_session_id)
                    && fence["runtime_launch_id"].as_str() == Some(incarnation)
            })
        })
    {
        return GroupCleanupProgressLiveness::Live;
    }
    let record = match crate::load_session_record(context, session_id) {
        Ok(record) => record,
        Err(error) if error.code() == "session-not-found" => {
            return GroupCleanupProgressLiveness::Stale(Box::new(snapshot));
        }
        Err(_) => return GroupCleanupProgressLiveness::Unverifiable,
    };
    if record.id == canonical_session_id
        && record
            .runtime
            .as_ref()
            .is_some_and(|runtime| runtime.launch_id == incarnation)
    {
        GroupCleanupProgressLiveness::Live
    } else {
        GroupCleanupProgressLiveness::Stale(Box::new(snapshot))
    }
}

fn stale_group_cleanup_progress_snapshot_matches(
    snapshot: &PrivateBoundedFile,
    path: &Path,
) -> bool {
    let Ok(descriptor_metadata) = snapshot.file.metadata() else {
        return false;
    };
    if !same_private_file_snapshot(&snapshot.snapshot, &descriptor_metadata) {
        return false;
    }
    let Ok(path_metadata) = fs::symlink_metadata(path) else {
        return false;
    };
    validate_private_file(&path_metadata).is_ok()
        && same_private_file_snapshot(&descriptor_metadata, &path_metadata)
}

fn recycle_stale_group_cleanup_progress_into_current(
    context: &CliContext,
    snapshot: &PrivateBoundedFile,
    source_path: &Path,
    current_path: &Path,
    incoming: &[u8],
) -> Result<bool, CliError> {
    if !stale_group_cleanup_progress_snapshot_matches(snapshot, source_path) {
        return Ok(false);
    }
    #[cfg(test)]
    pause_group_cleanup_progress_before_eviction_for_test(source_path);

    let prepared = prepare_group_cleanup_progress_recycle_slot(context, incoming)?;
    let recycle_path = group_cleanup_progress_recycle_path(context)?;
    if !stale_group_cleanup_progress_snapshot_matches(snapshot, source_path)
        || !stale_group_cleanup_progress_snapshot_matches(&prepared, &recycle_path)
    {
        retire_group_cleanup_progress_recycle_slot(context)?;
        return Ok(false);
    }
    let transaction = store_active_group_cleanup_progress_recycle_journal(
        context,
        source_path,
        Some(current_path),
        snapshot,
        &prepared,
    )?;
    #[cfg(test)]
    pause_group_cleanup_progress_before_exchange_for_test(&transaction.canonical_parent);
    transaction.fence_canonical_parent()?;
    transaction.fence_progress_parent()?;
    match rename_exchange_with_recycle_transaction(source_path, &transaction) {
        Ok(()) => {}
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            store_idle_group_cleanup_progress_recycle_journal_in_transaction(&transaction)?;
            retire_group_cleanup_progress_recycle_slot_in_transaction(&transaction)?;
            return Ok(false);
        }
        Err(_) => {
            store_idle_group_cleanup_progress_recycle_journal_in_transaction(&transaction)?;
            retire_group_cleanup_progress_recycle_slot_in_transaction(&transaction)?;
            return Err(store_unavailable());
        }
    }
    #[cfg(test)]
    pause_group_cleanup_progress_after_recycle_for_test(&recycle_path);
    #[cfg(test)]
    if group_cleanup_progress_crashes_after_recycle_for_test(&recycle_path) {
        return Err(store_unavailable());
    }

    let prepared_identity = group_cleanup_progress_snapshot_identity(&prepared);
    let source_identity = group_cleanup_progress_snapshot_identity(snapshot);
    if reconcile_group_cleanup_progress_exchange(
        source_path,
        &transaction,
        &prepared_identity,
        &source_identity,
    )? != GroupCleanupProgressExchangeReconciliation::Expected
    {
        transaction.sync_mutated_namespaces()?;
        store_idle_group_cleanup_progress_recycle_journal_in_transaction(&transaction)?;
        retire_group_cleanup_progress_recycle_slot_in_transaction(&transaction)?;
        return Ok(false);
    }
    let source = read_group_cleanup_progress_candidate_at(&transaction, source_path)?;
    if !source.as_ref().is_some_and(|source| {
        group_cleanup_progress_snapshot_identity_matches(&prepared_identity, source)
    }) {
        transaction.sync_mutated_namespaces()?;
        store_idle_group_cleanup_progress_recycle_journal_in_transaction(&transaction)?;
        retire_group_cleanup_progress_recycle_slot_in_transaction(&transaction)?;
        return Ok(false);
    }
    #[cfg(test)]
    pause_group_cleanup_progress_after_source_verify_for_test(source_path);
    match rename_noreplace_with_progress_transaction(source_path, current_path, &transaction) {
        Ok(()) => {}
        Err(_) => {
            rollback_group_cleanup_progress_exchange(
                source_path,
                &transaction,
                snapshot,
                &prepared,
            )?;
            transaction.sync_mutated_namespaces()?;
            store_idle_group_cleanup_progress_recycle_journal_in_transaction(&transaction)?;
            retire_group_cleanup_progress_recycle_slot_in_transaction(&transaction)?;
            return Err(store_unavailable());
        }
    }
    #[cfg(test)]
    pause_group_cleanup_progress_after_final_rename_for_test(current_path);
    #[cfg(test)]
    if group_cleanup_progress_crashes_after_final_rename_for_test(current_path) {
        return Err(store_unavailable());
    }
    let current = read_group_cleanup_progress_candidate_at(&transaction, current_path)?;
    if !current.as_ref().is_some_and(|current| {
        group_cleanup_progress_snapshot_identity_matches(&prepared_identity, current)
    }) {
        let recycle =
            read_group_cleanup_progress_recycle_slot_at(&transaction, MAX_REGISTRY_BYTES)?
                .ok_or_else(store_unavailable)?;
        if !group_cleanup_progress_snapshot_identity_matches(&source_identity, &recycle) {
            return Err(store_unavailable());
        }
        compensate_group_cleanup_progress_destination(
            &transaction,
            source_path,
            current_path,
            current.as_ref().ok_or_else(store_unavailable)?,
        )?;
        return Ok(false);
    }
    transaction.sync_mutated_namespaces()?;
    store_installed_group_cleanup_progress_recycle_journal_in_transaction(
        &transaction,
        source_path,
        current_path,
        &source_identity,
        &prepared_identity,
    )?;
    #[cfg(test)]
    pause_group_cleanup_progress_after_install_for_test(current_path);
    #[cfg(test)]
    if group_cleanup_progress_crashes_after_final_install_for_test(current_path) {
        return Err(store_unavailable());
    }
    store_idle_group_cleanup_progress_recycle_journal_in_transaction(&transaction)?;
    retire_group_cleanup_progress_recycle_slot_in_transaction(&transaction)?;
    Ok(true)
}

fn compact_stale_group_cleanup_progress(
    context: &CliContext,
    snapshot: &PrivateBoundedFile,
    source_path: &Path,
) -> Result<Option<u64>, CliError> {
    #[cfg(test)]
    GROUP_CLEANUP_PROGRESS_COMPACTIONS_FOR_TEST.set(
        GROUP_CLEANUP_PROGRESS_COMPACTIONS_FOR_TEST
            .get()
            .saturating_add(1),
    );
    if !stale_group_cleanup_progress_snapshot_matches(snapshot, source_path) {
        return Ok(None);
    }
    #[cfg(test)]
    pause_group_cleanup_progress_before_eviction_for_test(source_path);

    let retired = group_cleanup_progress_retired_receipt();
    let prepared = prepare_group_cleanup_progress_recycle_slot(context, retired)?;
    let recycle_path = group_cleanup_progress_recycle_path(context)?;
    if !stale_group_cleanup_progress_snapshot_matches(snapshot, source_path)
        || !stale_group_cleanup_progress_snapshot_matches(&prepared, &recycle_path)
    {
        retire_group_cleanup_progress_recycle_slot(context)?;
        return Ok(None);
    }
    let transaction = store_active_group_cleanup_progress_recycle_journal(
        context,
        source_path,
        None,
        snapshot,
        &prepared,
    )?;
    #[cfg(test)]
    pause_group_cleanup_progress_before_exchange_for_test(&transaction.canonical_parent);
    transaction.fence_canonical_parent()?;
    transaction.fence_progress_parent()?;
    match rename_exchange_with_recycle_transaction(source_path, &transaction) {
        Ok(()) => {}
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            store_idle_group_cleanup_progress_recycle_journal_in_transaction(&transaction)?;
            retire_group_cleanup_progress_recycle_slot_in_transaction(&transaction)?;
            return Ok(None);
        }
        Err(_) => {
            store_idle_group_cleanup_progress_recycle_journal_in_transaction(&transaction)?;
            retire_group_cleanup_progress_recycle_slot_in_transaction(&transaction)?;
            return Err(store_unavailable());
        }
    }
    #[cfg(test)]
    pause_group_cleanup_progress_after_recycle_for_test(&recycle_path);
    #[cfg(test)]
    if group_cleanup_progress_crashes_after_recycle_for_test(&recycle_path) {
        return Err(store_unavailable());
    }
    if reconcile_group_cleanup_progress_exchange(
        source_path,
        &transaction,
        &group_cleanup_progress_snapshot_identity(&prepared),
        &group_cleanup_progress_snapshot_identity(snapshot),
    )? != GroupCleanupProgressExchangeReconciliation::Expected
    {
        transaction.sync_mutated_namespaces()?;
        store_idle_group_cleanup_progress_recycle_journal_in_transaction(&transaction)?;
        retire_group_cleanup_progress_recycle_slot_in_transaction(&transaction)?;
        return Ok(None);
    }
    transaction.sync_mutated_namespaces()?;
    store_idle_group_cleanup_progress_recycle_journal_in_transaction(&transaction)?;
    retire_group_cleanup_progress_recycle_slot_in_transaction(&transaction)?;
    Ok(Some(retired.len() as u64))
}

fn rollback_group_cleanup_progress_exchange(
    source_path: &Path,
    transaction: &GroupCleanupProgressRecycleTransaction,
    source: &PrivateBoundedFile,
    prepared: &PrivateBoundedFile,
) -> Result<(), CliError> {
    rename_exchange_with_recycle_transaction(source_path, transaction)
        .map_err(|_| store_unavailable())?;
    reconcile_group_cleanup_progress_exchange(
        source_path,
        transaction,
        &group_cleanup_progress_snapshot_identity(source),
        &group_cleanup_progress_snapshot_identity(prepared),
    )
    .map(|_| ())
}

fn compensate_group_cleanup_progress_destination(
    transaction: &GroupCleanupProgressRecycleTransaction,
    source_path: &Path,
    current_path: &Path,
    displaced: &PrivateBoundedFile,
) -> Result<(), CliError> {
    let displaced_identity = group_cleanup_progress_snapshot_identity(displaced);
    rename_noreplace_with_progress_transaction(current_path, source_path, transaction)
        .map_err(|_| store_unavailable())?;
    let restored = read_group_cleanup_progress_candidate_at(transaction, source_path)?;
    let destination = read_group_cleanup_progress_candidate_at(transaction, current_path)?;
    if destination.is_some()
        || !restored.as_ref().is_some_and(|restored| {
            group_cleanup_progress_snapshot_identity_matches(&displaced_identity, restored)
        })
    {
        return Err(store_unavailable());
    }
    transaction.sync_mutated_namespaces()?;
    store_idle_group_cleanup_progress_recycle_journal_in_transaction(transaction)?;
    retire_group_cleanup_progress_recycle_slot_in_transaction(transaction)
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum GroupCleanupProgressExchangeReconciliation {
    Expected,
    Compensated,
    ReplacementPreserved,
}

fn reconcile_group_cleanup_progress_exchange(
    source_path: &Path,
    transaction: &GroupCleanupProgressRecycleTransaction,
    expected_source: &GroupCleanupProgressSnapshotIdentity,
    expected_recycle: &GroupCleanupProgressSnapshotIdentity,
) -> Result<GroupCleanupProgressExchangeReconciliation, CliError> {
    let source = read_group_cleanup_progress_candidate_at(transaction, source_path)?
        .ok_or_else(store_unavailable)?;
    let recycle = read_group_cleanup_progress_recycle_slot_at(transaction, MAX_REGISTRY_BYTES)?
        .ok_or_else(store_unavailable)?;
    if group_cleanup_progress_snapshot_identity_matches(expected_source, &source)
        && group_cleanup_progress_snapshot_identity_matches(expected_recycle, &recycle)
    {
        return Ok(GroupCleanupProgressExchangeReconciliation::Expected);
    }
    if !group_cleanup_progress_snapshot_identity_matches(expected_source, &source) {
        if group_cleanup_progress_snapshot_identity_matches(expected_recycle, &recycle) {
            return Ok(GroupCleanupProgressExchangeReconciliation::ReplacementPreserved);
        }
        return Err(store_unavailable());
    }

    let displaced = group_cleanup_progress_snapshot_identity(&recycle);
    rename_exchange_with_recycle_transaction(source_path, transaction)
        .map_err(|_| store_unavailable())?;
    let restored_source = read_group_cleanup_progress_candidate_at(transaction, source_path)?
        .ok_or_else(store_unavailable)?;
    let restored_recycle =
        read_group_cleanup_progress_recycle_slot_at(transaction, MAX_REGISTRY_BYTES)?
            .ok_or_else(store_unavailable)?;
    if !group_cleanup_progress_snapshot_identity_matches(&displaced, &restored_source)
        || !group_cleanup_progress_snapshot_identity_matches(expected_source, &restored_recycle)
    {
        return Err(store_unavailable());
    }
    Ok(GroupCleanupProgressExchangeReconciliation::Compensated)
}

fn group_cleanup_progress_recycle_path(context: &CliContext) -> Result<PathBuf, CliError> {
    let root = ensure_orchestration_root(context)?.join(GROUP_CLEANUP_PROGRESS_RECYCLE_DIR);
    ensure_private_directory(&root)?;
    Ok(root.join(GROUP_CLEANUP_PROGRESS_RECYCLE_FILE))
}

fn group_cleanup_progress_recycle_journal_path(context: &CliContext) -> Result<PathBuf, CliError> {
    let root = ensure_orchestration_root(context)?.join(GROUP_CLEANUP_PROGRESS_RECYCLE_DIR);
    ensure_private_directory(&root)?;
    Ok(root.join(GROUP_CLEANUP_PROGRESS_RECYCLE_JOURNAL_FILE))
}

fn group_cleanup_progress_snapshot_identity(
    snapshot: &PrivateBoundedFile,
) -> GroupCleanupProgressSnapshotIdentity {
    GroupCleanupProgressSnapshotIdentity {
        device: snapshot.snapshot.dev(),
        inode: snapshot.snapshot.ino(),
        length: snapshot.snapshot.len(),
        modified_seconds: snapshot.snapshot.mtime(),
        modified_nanoseconds: snapshot.snapshot.mtime_nsec(),
        sha256: hex(&Sha256::digest(&snapshot.bytes)),
    }
}

fn group_cleanup_progress_snapshot_identity_matches(
    identity: &GroupCleanupProgressSnapshotIdentity,
    snapshot: &PrivateBoundedFile,
) -> bool {
    identity.device == snapshot.snapshot.dev()
        && identity.inode == snapshot.snapshot.ino()
        && identity.length == snapshot.snapshot.len()
        && identity.modified_seconds == snapshot.snapshot.mtime()
        && identity.modified_nanoseconds == snapshot.snapshot.mtime_nsec()
        && identity.sha256 == hex(&Sha256::digest(&snapshot.bytes))
}

fn group_cleanup_progress_recycle_journal_bytes(
    journal: &GroupCleanupProgressRecycleJournalWire,
) -> Result<Vec<u8>, CliError> {
    serde_json::to_vec(journal).map_err(|_| store_unavailable())
}

struct GroupCleanupProgressRecycleTransaction {
    directory: File,
    canonical_parent: PathBuf,
    device: u64,
    inode: u64,
    progress_directory: Option<File>,
    canonical_progress_parent: Option<PathBuf>,
    progress_device: Option<u64>,
    progress_inode: Option<u64>,
}

impl GroupCleanupProgressRecycleTransaction {
    fn open(path: &Path) -> Result<Self, CliError> {
        let parent = path.parent().ok_or_else(store_unavailable)?;
        let directory = OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC)
            .open(parent)
            .map_err(|_| store_unavailable())?;
        let metadata = directory.metadata().map_err(|_| store_unavailable())?;
        if !metadata.is_dir()
            || metadata.uid() != unsafe { libc::geteuid() }
            || metadata.mode() & 0o077 != 0
        {
            return Err(store_invalid(
                "group cleanup progress recycle directory is unsafe",
            ));
        }
        Ok(Self {
            directory,
            canonical_parent: parent.to_path_buf(),
            device: metadata.dev(),
            inode: metadata.ino(),
            progress_directory: None,
            canonical_progress_parent: None,
            progress_device: None,
            progress_inode: None,
        })
    }

    fn open_with_progress(path: &Path, progress_parent: &Path) -> Result<Self, CliError> {
        let mut transaction = Self::open(path)?;
        let progress_directory = OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC)
            .open(progress_parent)
            .map_err(|_| store_unavailable())?;
        let metadata = progress_directory
            .metadata()
            .map_err(|_| store_unavailable())?;
        if !metadata.is_dir()
            || metadata.uid() != unsafe { libc::geteuid() }
            || metadata.mode() & 0o077 != 0
        {
            return Err(store_invalid("group cleanup progress directory is unsafe"));
        }
        transaction.progress_directory = Some(progress_directory);
        transaction.canonical_progress_parent = Some(progress_parent.to_path_buf());
        transaction.progress_device = Some(metadata.dev());
        transaction.progress_inode = Some(metadata.ino());
        Ok(transaction)
    }

    fn fence_canonical_parent(&self) -> Result<(), CliError> {
        let current =
            fs::symlink_metadata(&self.canonical_parent).map_err(|_| store_unavailable())?;
        if current.file_type().is_symlink()
            || !current.is_dir()
            || current.dev() != self.device
            || current.ino() != self.inode
        {
            return Err(store_unavailable());
        }
        Ok(())
    }

    fn fence_progress_parent(&self) -> Result<(), CliError> {
        let parent = self
            .canonical_progress_parent
            .as_ref()
            .ok_or_else(store_unavailable)?;
        let current = fs::symlink_metadata(parent).map_err(|_| store_unavailable())?;
        if current.file_type().is_symlink()
            || !current.is_dir()
            || Some(current.dev()) != self.progress_device
            || Some(current.ino()) != self.progress_inode
        {
            return Err(store_unavailable());
        }
        Ok(())
    }

    fn sync_mutated_namespaces(&self) -> Result<(), CliError> {
        #[cfg(test)]
        if group_cleanup_progress_directory_sync_fails_for_test(
            self.canonical_progress_parent
                .as_deref()
                .ok_or_else(store_unavailable)?,
        ) {
            return Err(store_unavailable());
        }
        self.progress_directory
            .as_ref()
            .ok_or_else(store_unavailable)?
            .sync_all()
            .map_err(|_| store_unavailable())?;
        self.directory.sync_all().map_err(|_| store_unavailable())?;
        self.fence_progress_parent()?;
        self.fence_canonical_parent()
    }

    fn store_journal(&self, bytes: &[u8]) -> Result<(), CliError> {
        let target_name =
            std::ffi::CString::new(GROUP_CLEANUP_PROGRESS_RECYCLE_JOURNAL_FILE.as_bytes())
                .map_err(|_| store_unavailable())?;
        let temporary_name = format!(
            ".{}.tmp-{}",
            GROUP_CLEANUP_PROGRESS_RECYCLE_JOURNAL_FILE,
            uuid::Uuid::new_v4().simple()
        );
        let temporary_name =
            std::ffi::CString::new(temporary_name).map_err(|_| store_unavailable())?;
        let temporary_fd = unsafe {
            libc::openat(
                self.directory.as_raw_fd(),
                temporary_name.as_ptr(),
                libc::O_WRONLY | libc::O_CREAT | libc::O_EXCL | libc::O_NOFOLLOW | libc::O_CLOEXEC,
                SECRET_FILE_MODE,
            )
        };
        if temporary_fd < 0 {
            return Err(store_unavailable());
        }
        let mut temporary = unsafe { File::from_raw_fd(temporary_fd) };
        let write_result = temporary
            .write_all(bytes)
            .and_then(|()| {
                let result = unsafe {
                    libc::fchmod(temporary.as_raw_fd(), SECRET_FILE_MODE as libc::mode_t)
                };
                if result == 0 {
                    Ok(())
                } else {
                    Err(std::io::Error::last_os_error())
                }
            })
            .and_then(|()| temporary.sync_all());
        if write_result.is_err() {
            drop(temporary);
            unsafe {
                libc::unlinkat(self.directory.as_raw_fd(), temporary_name.as_ptr(), 0);
            }
            return Err(store_unavailable());
        }
        drop(temporary);
        let renamed = unsafe {
            libc::renameat(
                self.directory.as_raw_fd(),
                temporary_name.as_ptr(),
                self.directory.as_raw_fd(),
                target_name.as_ptr(),
            )
        };
        if renamed != 0 {
            unsafe {
                libc::unlinkat(self.directory.as_raw_fd(), temporary_name.as_ptr(), 0);
            }
            return Err(store_unavailable());
        }
        #[cfg(test)]
        pause_group_cleanup_progress_journal_after_rename_for_test(&self.canonical_parent);
        #[cfg(test)]
        if group_cleanup_progress_journal_sync_fails_for_test(&self.canonical_parent) {
            return Err(store_unavailable());
        }
        self.directory.sync_all().map_err(|_| store_unavailable())?;
        self.fence_canonical_parent()
    }
}

#[cfg(test)]
fn store_group_cleanup_progress_recycle_journal_durable(
    path: &Path,
    bytes: &[u8],
) -> Result<GroupCleanupProgressRecycleTransaction, CliError> {
    let transaction = GroupCleanupProgressRecycleTransaction::open(path)?;
    transaction.store_journal(bytes)?;
    Ok(transaction)
}

#[cfg(test)]
fn store_idle_group_cleanup_progress_recycle_journal(context: &CliContext) -> Result<(), CliError> {
    let path = group_cleanup_progress_recycle_journal_path(context)?;
    let bytes =
        group_cleanup_progress_recycle_journal_bytes(&GroupCleanupProgressRecycleJournalWire {
            schema_version: GROUP_CLEANUP_PROGRESS_RECYCLE_JOURNAL_SCHEMA.to_string(),
            state: "idle".to_string(),
            phase: None,
            source_key: None,
            current_key: None,
            source: None,
            prepared: None,
        })?;
    store_group_cleanup_progress_recycle_journal_durable(&path, &bytes).map(drop)
}

fn store_idle_group_cleanup_progress_recycle_journal_in_transaction(
    transaction: &GroupCleanupProgressRecycleTransaction,
) -> Result<(), CliError> {
    let bytes =
        group_cleanup_progress_recycle_journal_bytes(&GroupCleanupProgressRecycleJournalWire {
            schema_version: GROUP_CLEANUP_PROGRESS_RECYCLE_JOURNAL_SCHEMA.to_string(),
            state: "idle".to_string(),
            phase: None,
            source_key: None,
            current_key: None,
            source: None,
            prepared: None,
        })?;
    transaction.store_journal(&bytes)
}

fn store_active_group_cleanup_progress_recycle_journal(
    context: &CliContext,
    source_path: &Path,
    current_path: Option<&Path>,
    source: &PrivateBoundedFile,
    prepared: &PrivateBoundedFile,
) -> Result<GroupCleanupProgressRecycleTransaction, CliError> {
    let source_key = source_path
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or_else(|| store_invalid("group cleanup progress key is invalid"))?;
    validate_slug("group cleanup progress key", source_key, 128)?;
    let current_key = current_path
        .map(|path| {
            let key = path
                .file_name()
                .and_then(|name| name.to_str())
                .ok_or_else(|| store_invalid("group cleanup progress key is invalid"))?;
            validate_slug("group cleanup progress key", key, 128)?;
            Ok::<_, CliError>(key.to_string())
        })
        .transpose()?;
    let path = group_cleanup_progress_recycle_journal_path(context)?;
    let progress_parent = source_path.parent().ok_or_else(store_unavailable)?;
    let bytes =
        group_cleanup_progress_recycle_journal_bytes(&GroupCleanupProgressRecycleJournalWire {
            schema_version: GROUP_CLEANUP_PROGRESS_RECYCLE_JOURNAL_SCHEMA.to_string(),
            state: "active".to_string(),
            phase: Some(GROUP_CLEANUP_PROGRESS_RECYCLE_PHASE_PREPARED.to_string()),
            source_key: Some(source_key.to_string()),
            current_key,
            source: Some(group_cleanup_progress_snapshot_identity(source)),
            prepared: Some(group_cleanup_progress_snapshot_identity(prepared)),
        })?;
    let transaction =
        GroupCleanupProgressRecycleTransaction::open_with_progress(&path, progress_parent)?;
    transaction.store_journal(&bytes)?;
    Ok(transaction)
}

fn store_installed_group_cleanup_progress_recycle_journal_in_transaction(
    transaction: &GroupCleanupProgressRecycleTransaction,
    source_path: &Path,
    current_path: &Path,
    source: &GroupCleanupProgressSnapshotIdentity,
    prepared: &GroupCleanupProgressSnapshotIdentity,
) -> Result<(), CliError> {
    let source_key = source_path
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or_else(store_unavailable)?;
    let current_key = current_path
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or_else(store_unavailable)?;
    validate_slug("group cleanup progress key", source_key, 128)
        .map_err(|_| store_unavailable())?;
    validate_slug("group cleanup progress key", current_key, 128)
        .map_err(|_| store_unavailable())?;
    let bytes =
        group_cleanup_progress_recycle_journal_bytes(&GroupCleanupProgressRecycleJournalWire {
            schema_version: GROUP_CLEANUP_PROGRESS_RECYCLE_JOURNAL_SCHEMA.to_string(),
            state: "active".to_string(),
            phase: Some(GROUP_CLEANUP_PROGRESS_RECYCLE_PHASE_INSTALLED.to_string()),
            source_key: Some(source_key.to_string()),
            current_key: Some(current_key.to_string()),
            source: Some(source.clone()),
            prepared: Some(prepared.clone()),
        })?;
    transaction.store_journal(&bytes)
}

fn recover_group_cleanup_progress_recycle(
    context: &CliContext,
    recycle_path: &Path,
) -> Result<(), CliError> {
    let progress_root = ensure_orchestration_root(context)?.join(GROUP_CLEANUP_PROGRESS_DIR);
    ensure_private_directory(&progress_root)?;
    let transaction =
        GroupCleanupProgressRecycleTransaction::open_with_progress(recycle_path, &progress_root)?;
    let Some(journal_snapshot) = read_group_cleanup_progress_file_at(
        &transaction.directory,
        GROUP_CLEANUP_PROGRESS_RECYCLE_JOURNAL_FILE,
        64 * 1024,
        "group cleanup progress recycle journal exceeds byte limit",
    )?
    else {
        return Ok(());
    };
    let state =
        serde_json::from_slice::<GroupCleanupProgressRecycleJournalWire>(&journal_snapshot.bytes)
            .map_err(|_| store_unavailable())?
            .into_state()?;
    let (installed, source_key, current_key, source_identity, prepared_identity) = match &state {
        GroupCleanupProgressRecycleState::Idle => {
            retire_group_cleanup_progress_recycle_slot_in_transaction(&transaction)?;
            return Ok(());
        }
        GroupCleanupProgressRecycleState::Prepared {
            source_key,
            current_key,
            source,
            prepared,
        } => (
            false,
            source_key.as_str(),
            current_key.as_deref(),
            source,
            prepared,
        ),
        GroupCleanupProgressRecycleState::Installed {
            source_key,
            current_key,
            source,
            prepared,
        } => (
            true,
            source_key.as_str(),
            Some(current_key.as_str()),
            source,
            prepared,
        ),
    };
    validate_slug("group cleanup progress key", source_key, 128)
        .map_err(|_| store_unavailable())?;
    let source_path = progress_root.join(source_key);
    let source = read_group_cleanup_progress_candidate_at(&transaction, &source_path)?;
    let recycle = read_group_cleanup_progress_recycle_slot_at(&transaction, MAX_REGISTRY_BYTES)?;

    if installed {
        let current_key = current_key.ok_or_else(store_unavailable)?;
        validate_slug("group cleanup progress key", current_key, 128)
            .map_err(|_| store_unavailable())?;
        let current_path = progress_root.join(current_key);
        let current = read_group_cleanup_progress_candidate_at(&transaction, &current_path)?;
        if source.is_none()
            && recycle.as_ref().is_some_and(|snapshot| {
                group_cleanup_progress_snapshot_identity_matches(source_identity, snapshot)
            })
            && current.as_ref().is_none_or(|snapshot| {
                !group_cleanup_progress_snapshot_identity_matches(source_identity, snapshot)
            })
        {
            transaction.sync_mutated_namespaces()?;
            store_idle_group_cleanup_progress_recycle_journal_in_transaction(&transaction)?;
            retire_group_cleanup_progress_recycle_slot_in_transaction(&transaction)?;
            return Ok(());
        }
        return Err(store_unavailable());
    }

    if source.as_ref().is_some_and(|snapshot| {
        group_cleanup_progress_snapshot_identity_matches(source_identity, snapshot)
    }) && recycle.as_ref().is_some_and(|snapshot| {
        group_cleanup_progress_snapshot_identity_matches(prepared_identity, snapshot)
    }) {
        store_idle_group_cleanup_progress_recycle_journal_in_transaction(&transaction)?;
        retire_group_cleanup_progress_recycle_slot_in_transaction(&transaction)?;
        return Ok(());
    }
    if source.as_ref().is_some_and(|snapshot| {
        group_cleanup_progress_snapshot_identity_matches(prepared_identity, snapshot)
    }) && recycle.as_ref().is_some_and(|snapshot| {
        group_cleanup_progress_snapshot_identity_matches(source_identity, snapshot)
    }) {
        #[cfg(test)]
        pause_group_cleanup_progress_recovery_before_exchange_for_test(&source_path);
        rename_exchange_with_recycle_transaction(&source_path, &transaction)
            .map_err(|_| store_unavailable())?;
        reconcile_group_cleanup_progress_exchange(
            &source_path,
            &transaction,
            source_identity,
            prepared_identity,
        )?;
        transaction.sync_mutated_namespaces()?;
        store_idle_group_cleanup_progress_recycle_journal_in_transaction(&transaction)?;
        retire_group_cleanup_progress_recycle_slot_in_transaction(&transaction)?;
        return Ok(());
    }
    if source.is_none()
        && current_key.is_none()
        && recycle.as_ref().is_some_and(|snapshot| {
            group_cleanup_progress_snapshot_identity_matches(source_identity, snapshot)
                || group_cleanup_progress_snapshot_identity_matches(prepared_identity, snapshot)
        })
    {
        transaction.sync_mutated_namespaces()?;
        store_idle_group_cleanup_progress_recycle_journal_in_transaction(&transaction)?;
        retire_group_cleanup_progress_recycle_slot_in_transaction(&transaction)?;
        return Ok(());
    }
    if source.is_none()
        && let Some(current_key) = current_key
    {
        validate_slug("group cleanup progress key", current_key, 128)
            .map_err(|_| store_unavailable())?;
        let current_path = progress_root.join(current_key);
        let current = read_group_cleanup_progress_candidate_at(&transaction, &current_path)?;
        if !recycle.as_ref().is_some_and(|snapshot| {
            group_cleanup_progress_snapshot_identity_matches(source_identity, snapshot)
        }) {
            return Err(store_unavailable());
        }
        if current.as_ref().is_some_and(|snapshot| {
            group_cleanup_progress_snapshot_identity_matches(prepared_identity, snapshot)
        }) || current.is_none()
        {
            transaction.sync_mutated_namespaces()?;
            store_idle_group_cleanup_progress_recycle_journal_in_transaction(&transaction)?;
            retire_group_cleanup_progress_recycle_slot_in_transaction(&transaction)?;
            return Ok(());
        }
        let displaced = current
            .as_ref()
            .filter(|snapshot| {
                !group_cleanup_progress_snapshot_identity_matches(source_identity, snapshot)
            })
            .ok_or_else(store_unavailable)?;
        compensate_group_cleanup_progress_destination(
            &transaction,
            &source_path,
            &current_path,
            displaced,
        )?;
        return Ok(());
    }
    if source.as_ref().is_some_and(|snapshot| {
        !group_cleanup_progress_snapshot_identity_matches(source_identity, snapshot)
            && !group_cleanup_progress_snapshot_identity_matches(prepared_identity, snapshot)
    }) && recycle.as_ref().is_some_and(|snapshot| {
        group_cleanup_progress_snapshot_identity_matches(source_identity, snapshot)
    }) {
        transaction.sync_mutated_namespaces()?;
        store_idle_group_cleanup_progress_recycle_journal_in_transaction(&transaction)?;
        retire_group_cleanup_progress_recycle_slot_in_transaction(&transaction)?;
        return Ok(());
    }
    Err(store_unavailable())
}

fn group_cleanup_progress_retired_receipt() -> &'static [u8] {
    static RECEIPT: std::sync::OnceLock<Vec<u8>> = std::sync::OnceLock::new();
    RECEIPT
        .get_or_init(|| {
            serde_json::to_vec(&GroupCleanupProgressReceipt {
                schema_version: GROUP_CLEANUP_PROGRESS_RECEIPT_V1_SCHEMA.to_string(),
                requested_session_id: None,
                principal_session_id: "retired-progress".to_string(),
                principal_incarnation: "retired-progress".to_string(),
                idempotency_key: "retired-progress".to_string(),
                request_digest: "0".repeat(64),
                outcome: json!({}),
            })
            .expect("static group cleanup progress receipt serializes")
        })
        .as_slice()
}

fn prepare_group_cleanup_progress_recycle_slot(
    context: &CliContext,
    desired: &[u8],
) -> Result<PrivateBoundedFile, CliError> {
    if desired.len() as u64 > MAX_REGISTRY_BYTES {
        return Err(store_invalid("group cleanup progress exceeds byte limit"));
    }
    let path = group_cleanup_progress_recycle_path(context)?;
    recover_group_cleanup_progress_recycle(context, &path)?;
    for _ in 0..2 {
        match read_private_bounded_file_with_limit(
            &path,
            MAX_REGISTRY_BYTES,
            "group cleanup progress recycle permissions are unsafe",
            "group cleanup progress recycle exceeds byte limit",
            "group cleanup progress recycle changed while it was being read",
        )? {
            Some(snapshot) if snapshot.bytes == desired => return Ok(snapshot),
            Some(snapshot) => {
                if !rewrite_group_cleanup_progress_snapshot(&snapshot, &path, desired)? {
                    return Err(store_unavailable());
                }
            }
            None => {
                let mut file = match OpenOptions::new()
                    .read(true)
                    .write(true)
                    .create_new(true)
                    .mode(SECRET_FILE_MODE)
                    .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC)
                    .open(&path)
                {
                    Ok(file) => file,
                    Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => continue,
                    Err(_) => return Err(store_unavailable()),
                };
                file.write_all(desired)
                    .and_then(|()| file.sync_all())
                    .map_err(|_| store_unavailable())?;
            }
        }
    }
    let snapshot = read_private_bounded_file_with_limit(
        &path,
        MAX_REGISTRY_BYTES,
        "group cleanup progress recycle permissions are unsafe",
        "group cleanup progress recycle exceeds byte limit",
        "group cleanup progress recycle changed while it was being read",
    )?
    .ok_or_else(store_unavailable)?;
    if snapshot.bytes != desired {
        return Err(store_unavailable());
    }
    Ok(snapshot)
}

fn retire_group_cleanup_progress_recycle_slot(context: &CliContext) -> Result<(), CliError> {
    prepare_group_cleanup_progress_recycle_slot(context, group_cleanup_progress_retired_receipt())
        .map(|_| ())
}

fn retire_group_cleanup_progress_recycle_slot_in_transaction(
    transaction: &GroupCleanupProgressRecycleTransaction,
) -> Result<(), CliError> {
    let retired = group_cleanup_progress_retired_receipt();
    let snapshot = read_group_cleanup_progress_recycle_slot_at(transaction, MAX_REGISTRY_BYTES)?
        .ok_or_else(store_unavailable)?;
    if snapshot.bytes == retired {
        return transaction.fence_canonical_parent();
    }
    if !rewrite_group_cleanup_progress_snapshot_at(&snapshot, transaction, retired)? {
        return Err(store_unavailable());
    }
    transaction.fence_canonical_parent()
}

fn rewrite_group_cleanup_progress_snapshot_at(
    snapshot: &PrivateBoundedFile,
    transaction: &GroupCleanupProgressRecycleTransaction,
    bytes: &[u8],
) -> Result<bool, CliError> {
    if !moved_group_cleanup_progress_snapshot_matches_at(snapshot, transaction) {
        return Ok(false);
    }
    let name = std::ffi::CString::new(GROUP_CLEANUP_PROGRESS_RECYCLE_FILE)
        .map_err(|_| store_unavailable())?;
    let descriptor = unsafe {
        libc::openat(
            transaction.directory.as_raw_fd(),
            name.as_ptr(),
            libc::O_RDWR | libc::O_NOFOLLOW | libc::O_CLOEXEC,
        )
    };
    if descriptor < 0 {
        return Err(store_unavailable());
    }
    let mut file = unsafe { File::from_raw_fd(descriptor) };
    let before = file.metadata().map_err(|_| store_unavailable())?;
    if before.dev() != snapshot.snapshot.dev() || before.ino() != snapshot.snapshot.ino() {
        return Ok(false);
    }
    file.set_len(0)
        .and_then(|()| file.seek(SeekFrom::Start(0)).map(|_| ()))
        .and_then(|()| file.write_all(bytes))
        .and_then(|()| file.sync_all())
        .map_err(|_| store_unavailable())?;
    let after = file.metadata().map_err(|_| store_unavailable())?;
    let observed = read_group_cleanup_progress_recycle_slot_at(transaction, MAX_REGISTRY_BYTES)?
        .ok_or_else(store_unavailable)?;
    Ok(observed.bytes == bytes
        && observed.snapshot.dev() == after.dev()
        && observed.snapshot.ino() == after.ino())
}

fn rewrite_group_cleanup_progress_snapshot(
    snapshot: &PrivateBoundedFile,
    path: &Path,
    bytes: &[u8],
) -> Result<bool, CliError> {
    if !stale_group_cleanup_progress_snapshot_matches(snapshot, path) {
        return Ok(false);
    }
    let mut file = OpenOptions::new()
        .read(true)
        .write(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC)
        .open(path)
        .map_err(|_| store_unavailable())?;
    let before = file.metadata().map_err(|_| store_unavailable())?;
    if !same_private_file_snapshot(&snapshot.snapshot, &before) {
        return Ok(false);
    }
    file.set_len(0)
        .and_then(|()| file.seek(SeekFrom::Start(0)).map(|_| ()))
        .and_then(|()| file.write_all(bytes))
        .and_then(|()| file.sync_all())
        .map_err(|_| store_unavailable())?;
    let after = file.metadata().map_err(|_| store_unavailable())?;
    let path_metadata = fs::symlink_metadata(path).map_err(|_| store_unavailable())?;
    if validate_private_file(&path_metadata).is_err()
        || !same_private_file_snapshot(&after, &path_metadata)
    {
        return Ok(false);
    }
    file.seek(SeekFrom::Start(0))
        .map_err(|_| store_unavailable())?;
    let mut observed = Vec::with_capacity(bytes.len());
    (&mut file)
        .take(MAX_REGISTRY_BYTES + 1)
        .read_to_end(&mut observed)
        .map_err(|_| store_unavailable())?;
    if observed != bytes {
        return Err(store_unavailable());
    }
    let stable = file.metadata().map_err(|_| store_unavailable())?;
    Ok(same_private_file_snapshot(&after, &stable))
}

fn read_group_cleanup_progress_file_at(
    directory: &File,
    name: &str,
    max_bytes: u64,
    oversized_message: &'static str,
) -> Result<Option<PrivateBoundedFile>, CliError> {
    let name = std::ffi::CString::new(name).map_err(|_| store_unavailable())?;
    let descriptor = unsafe {
        libc::openat(
            directory.as_raw_fd(),
            name.as_ptr(),
            libc::O_RDONLY | libc::O_NOFOLLOW | libc::O_CLOEXEC | libc::O_NONBLOCK,
        )
    };
    if descriptor < 0 {
        let error = std::io::Error::last_os_error();
        return if error.kind() == std::io::ErrorKind::NotFound {
            Ok(None)
        } else {
            Err(store_unavailable())
        };
    }
    let mut file = unsafe { File::from_raw_fd(descriptor) };
    let before = file.metadata().map_err(|_| store_unavailable())?;
    validate_private_file(&before)?;
    if before.len() > max_bytes {
        return Err(store_invalid(oversized_message));
    }
    let mut bytes = Vec::with_capacity(before.len() as usize);
    (&mut file)
        .take(max_bytes + 1)
        .read_to_end(&mut bytes)
        .map_err(|_| store_unavailable())?;
    let after = file.metadata().map_err(|_| store_unavailable())?;
    if bytes.len() as u64 > max_bytes
        || bytes.len() as u64 != after.len()
        || !same_private_file_snapshot(&before, &after)
    {
        return Err(store_unavailable());
    }
    Ok(Some(PrivateBoundedFile {
        file,
        bytes,
        snapshot: after,
    }))
}

fn read_group_cleanup_progress_recycle_slot_at(
    transaction: &GroupCleanupProgressRecycleTransaction,
    max_bytes: u64,
) -> Result<Option<PrivateBoundedFile>, CliError> {
    read_group_cleanup_progress_file_at(
        &transaction.directory,
        GROUP_CLEANUP_PROGRESS_RECYCLE_FILE,
        max_bytes,
        "group cleanup progress recycle exceeds byte limit",
    )
}

fn read_group_cleanup_progress_candidate_at(
    transaction: &GroupCleanupProgressRecycleTransaction,
    path: &Path,
) -> Result<Option<PrivateBoundedFile>, CliError> {
    let name = path
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or_else(store_unavailable)?;
    validate_slug("group cleanup progress key", name, 128).map_err(|_| store_unavailable())?;
    read_group_cleanup_progress_file_at(
        transaction
            .progress_directory
            .as_ref()
            .ok_or_else(store_unavailable)?,
        name,
        MAX_REGISTRY_BYTES,
        "group cleanup progress exceeds byte limit",
    )
}

fn moved_group_cleanup_progress_snapshot_matches_at(
    snapshot: &PrivateBoundedFile,
    transaction: &GroupCleanupProgressRecycleTransaction,
) -> bool {
    let Ok(before) = snapshot.file.metadata() else {
        return false;
    };
    if before.dev() != snapshot.snapshot.dev()
        || before.ino() != snapshot.snapshot.ino()
        || before.len() != snapshot.snapshot.len()
        || before.mtime() != snapshot.snapshot.mtime()
        || before.mtime_nsec() != snapshot.snapshot.mtime_nsec()
    {
        return false;
    }
    let Ok(Some(observed)) =
        read_group_cleanup_progress_recycle_slot_at(transaction, MAX_REGISTRY_BYTES)
    else {
        return false;
    };
    before.dev() == observed.snapshot.dev()
        && before.ino() == observed.snapshot.ino()
        && before.len() == observed.snapshot.len()
        && before.mtime() == observed.snapshot.mtime()
        && before.mtime_nsec() == observed.snapshot.mtime_nsec()
        && snapshot.bytes == observed.bytes
        && snapshot.file.metadata().is_ok_and(|after| {
            before.dev() == after.dev()
                && before.ino() == after.ino()
                && before.len() == after.len()
                && before.mtime() == after.mtime()
                && before.mtime_nsec() == after.mtime_nsec()
        })
}

fn rename_noreplace_with_progress_transaction(
    from: &Path,
    to: &Path,
    transaction: &GroupCleanupProgressRecycleTransaction,
) -> std::io::Result<()> {
    let progress_directory = transaction.progress_directory.as_ref().ok_or_else(|| {
        std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "progress directory is unavailable",
        )
    })?;
    let from = std::ffi::CString::new(
        from.file_name()
            .ok_or_else(|| std::io::Error::from(std::io::ErrorKind::InvalidInput))?
            .as_bytes(),
    )
    .map_err(|_| std::io::Error::from(std::io::ErrorKind::InvalidInput))?;
    let to = std::ffi::CString::new(
        to.file_name()
            .ok_or_else(|| std::io::Error::from(std::io::ErrorKind::InvalidInput))?
            .as_bytes(),
    )
    .map_err(|_| std::io::Error::from(std::io::ErrorKind::InvalidInput))?;
    #[cfg(target_os = "linux")]
    let result = unsafe {
        libc::syscall(
            libc::SYS_renameat2,
            progress_directory.as_raw_fd(),
            from.as_ptr(),
            progress_directory.as_raw_fd(),
            to.as_ptr(),
            libc::RENAME_NOREPLACE,
        )
    };
    #[cfg(target_os = "macos")]
    let result = unsafe {
        libc::renameatx_np(
            progress_directory.as_raw_fd(),
            from.as_ptr(),
            progress_directory.as_raw_fd(),
            to.as_ptr(),
            libc::RENAME_EXCL,
        ) as libc::c_long
    };
    #[cfg(not(any(target_os = "linux", target_os = "macos")))]
    let result: libc::c_long = return Err(std::io::Error::new(
        std::io::ErrorKind::Unsupported,
        "atomic no-replace rename is unavailable",
    ));
    if result == 0 {
        Ok(())
    } else {
        Err(std::io::Error::last_os_error())
    }
}

fn rename_exchange_with_recycle_transaction(
    source: &Path,
    transaction: &GroupCleanupProgressRecycleTransaction,
) -> std::io::Result<()> {
    let progress_directory = transaction.progress_directory.as_ref().ok_or_else(|| {
        std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "progress directory is unavailable",
        )
    })?;
    let source = std::ffi::CString::new(
        source
            .file_name()
            .ok_or_else(|| std::io::Error::from(std::io::ErrorKind::InvalidInput))?
            .as_bytes(),
    )
    .map_err(|_| std::io::Error::from(std::io::ErrorKind::InvalidInput))?;
    let slot = std::ffi::CString::new(GROUP_CLEANUP_PROGRESS_RECYCLE_FILE)
        .map_err(|_| std::io::Error::from(std::io::ErrorKind::InvalidInput))?;
    #[cfg(target_os = "linux")]
    let result = unsafe {
        libc::syscall(
            libc::SYS_renameat2,
            progress_directory.as_raw_fd(),
            source.as_ptr(),
            transaction.directory.as_raw_fd(),
            slot.as_ptr(),
            libc::RENAME_EXCHANGE,
        )
    };
    #[cfg(target_os = "macos")]
    let result = unsafe {
        libc::renameatx_np(
            progress_directory.as_raw_fd(),
            source.as_ptr(),
            transaction.directory.as_raw_fd(),
            slot.as_ptr(),
            libc::RENAME_SWAP,
        ) as libc::c_long
    };
    #[cfg(not(any(target_os = "linux", target_os = "macos")))]
    let result: libc::c_long = return Err(std::io::Error::new(
        std::io::ErrorKind::Unsupported,
        "atomic exchange rename is unavailable",
    ));
    if result == 0 {
        Ok(())
    } else {
        Err(std::io::Error::last_os_error())
    }
}

pub(crate) fn read_group_cleanup_progress(
    context: &CliContext,
    key: &str,
) -> Result<Option<Vec<u8>>, CliError> {
    let path = group_cleanup_progress_path(context, key)?;
    read_private_bounded_file_with_limit(
        &path,
        MAX_REGISTRY_BYTES,
        "group cleanup progress permissions are unsafe",
        "group cleanup progress exceeds byte limit",
        "group cleanup progress changed while it was being read",
    )
    .map(|snapshot| snapshot.map(|snapshot| snapshot.bytes))
}

pub(crate) fn recover_group_cleanup_progress_principal(
    context: &CliContext,
    requested_session_id: &str,
    incarnation: &str,
    idempotency_key: &str,
    request_digest: &str,
) -> Result<Option<String>, CliError> {
    let _lock = lock_group_cleanup_progress(context)?;
    let root = ensure_orchestration_root(context)?.join(GROUP_CLEANUP_PROGRESS_DIR);
    ensure_private_directory(&root)?;
    let recycle_path = group_cleanup_progress_recycle_path(context)?;
    recover_group_cleanup_progress_recycle(context, &recycle_path)?;
    let mut paths = Vec::new();
    let mut aggregate_bytes = 0_u64;
    for entry in fs::read_dir(&root).map_err(|_| store_unavailable())? {
        #[cfg(test)]
        count_group_cleanup_progress_visit_for_test(&root);
        let entry = entry.map_err(|_| store_unavailable())?;
        let key = entry
            .file_name()
            .into_string()
            .map_err(|_| store_invalid("group cleanup progress key is invalid"))?;
        validate_slug("group cleanup progress key", &key, 128)?;
        let path = entry.path();
        let metadata = fs::symlink_metadata(&path).map_err(|_| store_unavailable())?;
        validate_private_file(&metadata)?;
        if metadata.len() > MAX_REGISTRY_BYTES {
            return Err(store_invalid("group cleanup progress exceeds byte limit"));
        }
        aggregate_bytes = aggregate_bytes
            .checked_add(metadata.len())
            .ok_or_else(|| store_invalid("group cleanup progress aggregate is invalid"))?;
        paths.push(path);
        if paths.len() > MAX_GROUP_CLEANUP_PROGRESS_FILES
            || aggregate_bytes > MAX_GROUP_CLEANUP_PROGRESS_BYTES
        {
            return Err(group_cleanup_progress_capacity());
        }
    }

    let mut recovered = None;
    for path in paths {
        let snapshot = read_private_bounded_file_with_limit(
            &path,
            MAX_REGISTRY_BYTES,
            "group cleanup progress permissions are unsafe",
            "group cleanup progress exceeds byte limit",
            "group cleanup progress changed while it was being read",
        )?
        .ok_or_else(store_unavailable)?;
        let value = parse_group_cleanup_progress_receipt_value(&snapshot.bytes)
            .map_err(|_| store_invalid("group cleanup progress is invalid"))?;
        if !matches!(
            value["schema_version"].as_str(),
            Some(GROUP_CLEANUP_PROGRESS_RECEIPT_SCHEMA | GROUP_CLEANUP_PROGRESS_RECEIPT_V1_SCHEMA)
        ) || value["principal_incarnation"].as_str() != Some(incarnation)
            || value["idempotency_key"].as_str() != Some(idempotency_key)
            || value["request_digest"].as_str() != Some(request_digest)
        {
            continue;
        }
        let receipt = decode_group_cleanup_progress_receipt_value(value)
            .map_err(|_| store_invalid("group cleanup progress is invalid"))?;
        let receipt_session_id = receipt.principal_session_id.as_str();
        let Some(canonical_session_id) =
            receipt.outcome["_resume"]["plan"]["main"]["session_id"].as_str()
        else {
            continue;
        };
        let Some(canonical_incarnation) =
            receipt.outcome["_resume"]["plan"]["main"]["session_incarnation"].as_str()
        else {
            continue;
        };
        crate::validate_id(canonical_session_id)
            .map_err(|_| store_invalid("group cleanup progress identity is invalid"))?;
        if !(GroupCleanupSelectorBinding {
            schema_version: &receipt.schema_version,
            requested_session_id: receipt.requested_session_id.as_deref(),
            stored_principal_session_id: receipt_session_id,
            canonical_session_id,
            stored_incarnation: &receipt.principal_incarnation,
            canonical_incarnation,
            expected_session_id: requested_session_id,
            expected_incarnation: incarnation,
        })
        .is_exact()
            || !receipt.outcome["_resume"]["pending_registry_fences"]
                .as_array()
                .is_some_and(|fences| {
                    fences.iter().any(|fence| {
                        fence["session_id"].as_str() == Some(canonical_session_id)
                            && fence["runtime_launch_id"].as_str() == Some(incarnation)
                    })
                })
        {
            continue;
        }
        if recovered
            .as_deref()
            .is_some_and(|existing| existing != canonical_session_id)
        {
            return Err(CliError::data(
                "group-cleanup-progress-conflict",
                "multiple resumable cleanup principals matched the requested session alias",
                None,
            ));
        }
        recovered = Some(canonical_session_id.to_string());
    }
    Ok(recovered)
}

fn group_cleanup_progress_capacity() -> CliError {
    CliError::unavailable(
        "group-cleanup-progress-capacity",
        "active resumable group cleanup progress reached its aggregate capacity",
        Some(json!({
            "max_files": MAX_GROUP_CLEANUP_PROGRESS_FILES,
            "max_bytes": MAX_GROUP_CLEANUP_PROGRESS_BYTES
        })),
    )
}

pub(crate) fn remove_group_cleanup_progress(
    context: &CliContext,
    key: &str,
) -> Result<(), CliError> {
    let path = group_cleanup_progress_path(context, key)?;
    let _lock = lock_group_cleanup_progress(context)?;
    match fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(_) => Err(store_unavailable()),
    }
}

pub(crate) fn store_packet(context: &CliContext, value: &Value) -> Result<String, CliError> {
    let bytes = serde_json::to_vec(value)
        .map_err(|_| store_invalid("private orchestration packet is invalid"))?;
    if bytes.len() > 256 * 1024 {
        return Err(store_invalid(
            "private orchestration packet exceeds byte limit",
        ));
    }
    let digest = packet_digest_bytes(&bytes);
    let path = packet_path(context, &digest)?;
    match read_private_bounded_file_with_limit(
        &path,
        256 * 1024,
        "private orchestration packet permissions are unsafe",
        "private orchestration packet exceeds byte limit",
        "private orchestration packet changed while it was being read",
    )? {
        Some(existing) => {
            if existing.bytes != bytes {
                return Err(store_invalid(
                    "private orchestration packet digest collision",
                ));
            }
        }
        None => {
            write_atomic(&path, &bytes, SECRET_FILE_MODE).map_err(|_| store_unavailable())?;
        }
    }
    Ok(digest)
}

pub(crate) fn packet_digest(value: &Value) -> Result<String, CliError> {
    let bytes = serde_json::to_vec(value)
        .map_err(|_| store_invalid("private orchestration packet is invalid"))?;
    if bytes.len() > 256 * 1024 {
        return Err(store_invalid(
            "private orchestration packet exceeds byte limit",
        ));
    }
    Ok(packet_digest_bytes(&bytes))
}

pub(crate) fn read_packet(context: &CliContext, digest: &str) -> Result<Value, CliError> {
    let path = packet_path(context, digest)?;
    let snapshot = read_private_bounded_file_with_limit(
        &path,
        256 * 1024,
        "private orchestration packet permissions are unsafe",
        "private orchestration packet exceeds byte limit",
        "private orchestration packet changed while it was being read",
    )?
    .ok_or_else(store_unavailable)?;
    let actual = packet_digest_bytes(&snapshot.bytes);
    if actual != digest {
        return Err(store_invalid(
            "private orchestration packet digest is invalid",
        ));
    }
    serde_json::from_slice(&snapshot.bytes)
        .map_err(|_| store_invalid("private orchestration packet is invalid"))
}

fn orchestration_root(context: &CliContext) -> PathBuf {
    context.state_dir.join(ORCHESTRATION_DIR)
}

fn hex(bytes: &[u8]) -> String {
    const DIGITS: &[u8; 16] = b"0123456789abcdef";
    let mut encoded = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        encoded.push(DIGITS[(byte >> 4) as usize] as char);
        encoded.push(DIGITS[(byte & 0x0f) as usize] as char);
    }
    encoded
}

fn packet_digest_bytes(bytes: &[u8]) -> String {
    format!("sha256:{}", hex(&Sha256::digest(bytes)))
}

pub(crate) fn ensure_orchestration_root(context: &CliContext) -> Result<PathBuf, CliError> {
    let root = orchestration_root(context);
    ensure_private_directory(&root)?;
    Ok(root)
}

fn ensure_private_directory(path: &Path) -> Result<(), CliError> {
    match fs::create_dir(path) {
        Ok(()) => {}
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
        Err(_) => return Err(store_unavailable()),
    }
    let metadata = fs::symlink_metadata(path).map_err(|_| store_unavailable())?;
    if metadata.file_type().is_symlink()
        || !metadata.is_dir()
        || metadata.uid() != unsafe { libc::geteuid() }
    {
        return Err(store_invalid("orchestration store root is unsafe"));
    }
    fs::set_permissions(path, fs::Permissions::from_mode(0o700))
        .map_err(|_| store_unavailable())?;
    Ok(())
}

fn validate_private_file(metadata: &fs::Metadata) -> Result<(), CliError> {
    if metadata.file_type().is_symlink()
        || !metadata.is_file()
        || metadata.uid() != unsafe { libc::geteuid() }
        || metadata.mode() & 0o077 != 0
    {
        return Err(store_invalid(
            "orchestration registry permissions are unsafe",
        ));
    }
    Ok(())
}

fn validate_session_ref(reference: &SessionRef) -> Result<(), CliError> {
    crate::validate_id(&reference.session_id)?;
    validate_slug("session incarnation", &reference.session_incarnation, 128)?;
    if reference.session_created_at.trim().is_empty() || reference.session_created_at.len() > 64 {
        return Err(store_invalid("session reference timestamp is invalid"));
    }
    if let Some(machine) = &reference.machine {
        crate::validate_host(machine)?;
    }
    Ok(())
}

pub(crate) fn validate_slug(name: &str, value: &str, max: usize) -> Result<(), CliError> {
    if value.is_empty()
        || value.len() > max
        || !value
            .chars()
            .all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '-' | '_' | '.' | ':'))
    {
        return Err(store_invalid(&format!("{name} is invalid")));
    }
    Ok(())
}

fn validate_state(value: &str, allowed: &[&str]) -> Result<(), CliError> {
    if !allowed.contains(&value) {
        return Err(store_invalid("orchestration state is unsupported"));
    }
    Ok(())
}

pub(crate) fn validate_summary(name: &str, value: &str) -> Result<(), CliError> {
    if value.trim().is_empty() || value.chars().count() > 240 || value.chars().any(char::is_control)
    {
        return Err(store_invalid(&format!("{name} is invalid")));
    }
    Ok(())
}

pub(crate) fn validate_digest(value: &str) -> Result<(), CliError> {
    let Some(hex) = value.strip_prefix("sha256:") else {
        return Err(store_invalid("packet digest is invalid"));
    };
    if hex.len() != 64
        || !hex
            .chars()
            .all(|ch| ch.is_ascii_hexdigit() && !ch.is_ascii_uppercase())
    {
        return Err(store_invalid("packet digest is invalid"));
    }
    Ok(())
}

fn store_invalid(message: &str) -> CliError {
    CliError::data("orchestration-store-invalid", message, None)
}

fn store_unavailable() -> CliError {
    CliError::unavailable(
        "orchestration-store-unavailable",
        "orchestration store is unavailable",
        Some(json!({ "retryable": true })),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn group_cleanup_progress_receipt_decoder_enforces_one_v1_v2_contract() {
        let receipt = |schema_version: &str, requested_session_id: Option<Value>| {
            let mut value = json!({
                "schema_version": schema_version,
                "principal_session_id": "main-controller",
                "principal_incarnation": "main-incarnation",
                "idempotency_key": "cleanup-key",
                "request_digest": "a".repeat(64),
                "outcome": {}
            });
            if let Some(requested_session_id) = requested_session_id {
                value["requested_session_id"] = requested_session_id;
            }
            serde_json::to_vec(&value).unwrap()
        };
        assert!(
            decode_group_cleanup_progress_receipt(&receipt(
                GROUP_CLEANUP_PROGRESS_RECEIPT_V1_SCHEMA,
                None,
            ))
            .is_ok()
        );
        assert!(
            decode_group_cleanup_progress_receipt(&receipt(
                GROUP_CLEANUP_PROGRESS_RECEIPT_V1_SCHEMA,
                Some(Value::Null),
            ))
            .is_err(),
            "v1 requires the selector field to be absent"
        );
        assert!(
            decode_group_cleanup_progress_receipt(&receipt(
                GROUP_CLEANUP_PROGRESS_RECEIPT_SCHEMA,
                Some(Value::String("main-c".to_string())),
            ))
            .is_ok()
        );
        assert!(
            decode_group_cleanup_progress_receipt(&receipt(
                GROUP_CLEANUP_PROGRESS_RECEIPT_SCHEMA,
                None,
            ))
            .is_err(),
            "v2 requires one valid exact requested selector"
        );
        let mut unknown = serde_json::from_slice::<Value>(&receipt(
            GROUP_CLEANUP_PROGRESS_RECEIPT_SCHEMA,
            Some(Value::String("main-c".to_string())),
        ))
        .unwrap();
        unknown["unexpected"] = Value::Bool(true);
        assert!(
            decode_group_cleanup_progress_receipt(&serde_json::to_vec(&unknown).unwrap()).is_err(),
            "every reader must reject unknown receipt fields"
        );
    }

    #[test]
    fn group_cleanup_selector_binding_enforces_one_exact_v1_v2_policy() {
        let exact = |schema_version,
                     requested_session_id,
                     stored_principal_session_id,
                     canonical_session_id,
                     stored_incarnation,
                     canonical_incarnation,
                     expected_session_id,
                     expected_incarnation| {
            GroupCleanupSelectorBinding {
                schema_version,
                requested_session_id,
                stored_principal_session_id,
                canonical_session_id,
                stored_incarnation,
                canonical_incarnation,
                expected_session_id,
                expected_incarnation,
            }
            .is_exact()
        };

        assert!(exact(
            GROUP_CLEANUP_PROGRESS_RECEIPT_V1_SCHEMA,
            None,
            "main-c",
            "main-controller-unique",
            "main-incarnation",
            "main-incarnation",
            "main-c",
            "main-incarnation",
        ));
        assert!(!exact(
            GROUP_CLEANUP_PROGRESS_RECEIPT_V1_SCHEMA,
            Some("main-c"),
            "main-c",
            "main-controller-unique",
            "main-incarnation",
            "main-incarnation",
            "main-c",
            "main-incarnation",
        ));
        assert!(!exact(
            GROUP_CLEANUP_PROGRESS_RECEIPT_V1_SCHEMA,
            None,
            "main-c",
            "main-controller-unique",
            "main-incarnation",
            "main-incarnation",
            "main-cat",
            "main-incarnation",
        ));
        assert!(exact(
            GROUP_CLEANUP_PROGRESS_RECEIPT_SCHEMA,
            Some("main-c"),
            "main-controller-unique",
            "main-controller-unique",
            "main-incarnation",
            "main-incarnation",
            "main-c",
            "main-incarnation",
        ));
        assert!(!exact(
            GROUP_CLEANUP_PROGRESS_RECEIPT_SCHEMA,
            Some("main-cont"),
            "main-controller-unique",
            "main-controller-unique",
            "main-incarnation",
            "main-incarnation",
            "main-c",
            "main-incarnation",
        ));
        assert!(!exact(
            GROUP_CLEANUP_PROGRESS_RECEIPT_SCHEMA,
            Some("main-c"),
            "main-c",
            "main-controller-unique",
            "main-incarnation",
            "main-incarnation",
            "main-c",
            "main-incarnation",
        ));
        assert!(!exact(
            GROUP_CLEANUP_PROGRESS_RECEIPT_SCHEMA,
            Some("main-c"),
            "main-controller-unique",
            "main-controller-unique",
            "other-incarnation",
            "main-incarnation",
            "main-c",
            "main-incarnation",
        ));
    }

    #[test]
    fn group_cleanup_progress_recycle_journal_decodes_a_typed_state_matrix() {
        let identity = GroupCleanupProgressSnapshotIdentity {
            device: 1,
            inode: 2,
            length: 3,
            modified_seconds: 4,
            modified_nanoseconds: 5,
            sha256: "a".repeat(64),
        };
        let decode = |value: Value| {
            serde_json::from_value::<GroupCleanupProgressRecycleJournalWire>(value)
                .unwrap()
                .into_state()
        };
        let active = |phase: Option<&str>, current_key: Option<&str>| {
            json!({
                "schema_version": GROUP_CLEANUP_PROGRESS_RECYCLE_JOURNAL_SCHEMA,
                "state": "active",
                "phase": phase,
                "source_key": "source",
                "current_key": current_key,
                "source": identity,
                "prepared": identity,
            })
        };

        assert!(matches!(
            decode(json!({
                "schema_version": GROUP_CLEANUP_PROGRESS_RECYCLE_JOURNAL_SCHEMA,
                "state": "idle",
            }))
            .unwrap(),
            GroupCleanupProgressRecycleState::Idle
        ));
        assert!(matches!(
            decode(active(None, Some("current"))).unwrap(),
            GroupCleanupProgressRecycleState::Prepared { .. }
        ));
        assert!(matches!(
            decode(active(
                Some(GROUP_CLEANUP_PROGRESS_RECYCLE_PHASE_PREPARED),
                None,
            ))
            .unwrap(),
            GroupCleanupProgressRecycleState::Prepared { .. }
        ));
        assert!(matches!(
            decode(active(
                Some(GROUP_CLEANUP_PROGRESS_RECYCLE_PHASE_INSTALLED),
                Some("current"),
            ))
            .unwrap(),
            GroupCleanupProgressRecycleState::Installed { .. }
        ));
        assert!(
            decode(active(
                Some(GROUP_CLEANUP_PROGRESS_RECYCLE_PHASE_INSTALLED),
                None,
            ))
            .is_err()
        );
        assert!(decode(active(Some("unknown"), Some("current"))).is_err());
        assert!(
            decode(json!({
                "schema_version": GROUP_CLEANUP_PROGRESS_RECYCLE_JOURNAL_SCHEMA,
                "state": "idle",
                "source_key": "unexpected",
            }))
            .is_err()
        );
        assert!(
            decode(json!({
                "schema_version": "agent-session.group-cleanup-progress-recycle.v0",
                "state": "idle",
            }))
            .is_err()
        );
    }

    fn arm_private_read_replacement(path: &Path, replacement: &'static str, max_bytes: u64) {
        let replacement_bytes = fs::read(path).expect("private target bytes");
        let outside = path.with_extension(format!("{replacement}.outside"));
        PRIVATE_READ_REPLACEMENT_HOOK.with(|slot| {
            *slot.borrow_mut() = Some(Box::new(move |path| {
                fs::remove_file(path).expect("unlink validated private target");
                match replacement {
                    "symlink" => {
                        fs::write(&outside, &replacement_bytes).expect("outside private target");
                        fs::set_permissions(&outside, fs::Permissions::from_mode(0o600))
                            .expect("outside private target mode");
                        std::os::unix::fs::symlink(&outside, path).expect("replacement symlink");
                    }
                    "fifo" => {
                        use std::ffi::CString;
                        use std::os::unix::ffi::OsStrExt;

                        let fifo = CString::new(path.as_os_str().as_bytes())
                            .expect("fifo private target path");
                        assert_eq!(unsafe { libc::mkfifo(fifo.as_ptr(), SECRET_FILE_MODE) }, 0);
                        let fifo_path = path.to_path_buf();
                        thread::spawn(move || {
                            let deadline = Instant::now() + Duration::from_millis(500);
                            while Instant::now() < deadline {
                                match OpenOptions::new().write(true).open(&fifo_path) {
                                    Ok(mut writer) => {
                                        use std::io::Write;
                                        let _ = writer.write_all(&replacement_bytes);
                                        return;
                                    }
                                    Err(_) => thread::sleep(Duration::from_millis(1)),
                                }
                            }
                        });
                    }
                    "oversized" => {
                        let mut oversized = replacement_bytes;
                        oversized.extend(std::iter::repeat_n(b' ', max_bytes as usize + 1));
                        fs::write(path, oversized).expect("oversized replacement");
                        fs::set_permissions(path, fs::Permissions::from_mode(0o600))
                            .expect("oversized replacement mode");
                    }
                    _ => unreachable!(),
                }
            }));
        });
    }

    #[test]
    fn private_authority_reader_rejects_replacement_after_validation() {
        for replacement in ["symlink", "fifo", "oversized"] {
            let tmp = tempfile::TempDir::new().expect("tempdir");
            let context = CliContext {
                state_dir: tmp.path().join("state"),
                host: None,
            };
            let worker = SessionRef {
                machine: None,
                session_id: "worker".to_string(),
                session_incarnation: "worker-incarnation".to_string(),
                session_created_at: "2030-01-01T00:00:00Z".to_string(),
            };
            let main = SessionRef {
                machine: None,
                session_id: "main".to_string(),
                session_incarnation: "main-incarnation".to_string(),
                session_created_at: "2030-01-01T00:00:00Z".to_string(),
            };
            let session = crate::session_dir(&context, &worker.session_id);
            fs::create_dir_all(&session).expect("session directory");
            fs::set_permissions(&session, fs::Permissions::from_mode(0o700))
                .expect("private session directory");
            persist_session_group_cleanup_fence(
                &context,
                &worker,
                &main,
                "run-one",
                &format!("sha256:{}", "a".repeat(64)),
            )
            .expect("group cleanup fence");
            let marker = session.join(SESSION_GROUP_CLEANUP_FENCE_FILE);
            let bytes = fs::read(&marker).expect("marker bytes");
            let outside = tmp.path().join(format!("{replacement}-outside"));
            let replacement_bytes = bytes.clone();
            PRIVATE_READ_REPLACEMENT_HOOK.with(|slot| {
                *slot.borrow_mut() = Some(Box::new(move |path| {
                    fs::remove_file(path).expect("unlink validated marker");
                    match replacement {
                        "symlink" => {
                            fs::write(&outside, &replacement_bytes).expect("outside marker");
                            fs::set_permissions(&outside, fs::Permissions::from_mode(0o600))
                                .expect("outside marker mode");
                            std::os::unix::fs::symlink(&outside, path)
                                .expect("replacement symlink");
                        }
                        "fifo" => {
                            use std::ffi::CString;
                            use std::os::unix::ffi::OsStrExt;

                            let fifo = CString::new(path.as_os_str().as_bytes())
                                .expect("fifo marker path");
                            assert_eq!(unsafe { libc::mkfifo(fifo.as_ptr(), SECRET_FILE_MODE) }, 0);
                            let fifo_path = path.to_path_buf();
                            thread::spawn(move || {
                                let deadline = Instant::now() + Duration::from_millis(500);
                                while Instant::now() < deadline {
                                    match OpenOptions::new().write(true).open(&fifo_path) {
                                        Ok(mut writer) => {
                                            use std::io::Write;
                                            let _ = writer.write_all(&replacement_bytes);
                                            return;
                                        }
                                        Err(_) => thread::sleep(Duration::from_millis(1)),
                                    }
                                }
                            });
                        }
                        "oversized" => {
                            let mut oversized = replacement_bytes;
                            oversized.extend(std::iter::repeat_n(
                                b' ',
                                MAX_SESSION_AUTHORITY_QUARANTINE_BYTES as usize + 1,
                            ));
                            fs::write(path, oversized).expect("oversized replacement");
                            fs::set_permissions(path, fs::Permissions::from_mode(0o600))
                                .expect("oversized replacement mode");
                        }
                        _ => unreachable!(),
                    }
                }));
            });
            let started = Instant::now();
            let error = read_session_group_cleanup_fence(&context, &worker.session_id)
                .expect_err("path replacement must fail closed");
            assert_eq!(error.code(), "orchestration-store-invalid");
            assert!(
                started.elapsed() < Duration::from_secs(1),
                "{replacement} replacement must fail promptly"
            );
        }
    }

    #[test]
    fn descriptor_bound_reader_rejects_replacements_for_quarantine_and_packet_paths() {
        for target in ["quarantine", "existing-packet-storage", "packet-read"] {
            for replacement in ["symlink", "fifo", "oversized"] {
                let tmp = tempfile::TempDir::new().expect("tempdir");
                let context = CliContext {
                    state_dir: tmp.path().join("state"),
                    host: None,
                };
                fs::create_dir(&context.state_dir).expect("state directory");
                fs::set_permissions(&context.state_dir, fs::Permissions::from_mode(0o700))
                    .expect("private state directory");
                let worker = SessionRef {
                    machine: None,
                    session_id: "worker".to_string(),
                    session_incarnation: "worker-incarnation".to_string(),
                    session_created_at: "2030-01-01T00:00:00Z".to_string(),
                };
                let packet = json!({
                    "schema_version": "replacement-packet.v1",
                    "target": target
                });
                let (path, max_bytes, packet_digest) = match target {
                    "quarantine" => {
                        let session = crate::session_dir(&context, &worker.session_id);
                        fs::create_dir_all(&session).expect("session directory");
                        fs::set_permissions(&session, fs::Permissions::from_mode(0o700))
                            .expect("private session directory");
                        let quarantine = WorkerQuarantineRecord {
                            schema_version: WORKER_QUARANTINE_SCHEMA.to_string(),
                            worker: worker.clone(),
                            reason: "replacement regression".to_string(),
                            runtime_identity_digest: format!("sha256:{}", "a".repeat(64)),
                            created_at: "2030-01-01T00:00:01Z".to_string(),
                        };
                        persist_session_authority_quarantine(
                            &context,
                            "assignment-one",
                            2,
                            &quarantine,
                        )
                        .expect("quarantine marker");
                        (
                            session.join(SESSION_AUTHORITY_QUARANTINE_FILE),
                            MAX_SESSION_AUTHORITY_QUARANTINE_BYTES,
                            None,
                        )
                    }
                    "existing-packet-storage" | "packet-read" => {
                        let digest = store_packet(&context, &packet).expect("packet");
                        (
                            packet_path(&context, &digest).expect("packet path"),
                            256 * 1024,
                            Some(digest),
                        )
                    }
                    _ => unreachable!(),
                };
                arm_private_read_replacement(&path, replacement, max_bytes);
                let started = Instant::now();
                let error = match target {
                    "quarantine" => read_session_authority_quarantine(&context, &worker.session_id)
                        .map(|_| ())
                        .expect_err("quarantine replacement must fail closed"),
                    "existing-packet-storage" => store_packet(&context, &packet)
                        .map(|_| ())
                        .expect_err("existing packet replacement must fail closed"),
                    "packet-read" => {
                        read_packet(&context, packet_digest.as_deref().expect("packet digest"))
                            .map(|_| ())
                            .expect_err("packet read replacement must fail closed")
                    }
                    _ => unreachable!(),
                };
                assert_eq!(error.code(), "orchestration-store-invalid");
                assert!(
                    started.elapsed() < Duration::from_secs(1),
                    "{target} {replacement} replacement must fail promptly"
                );
            }
        }
    }

    // Frozen copies of the exact serde-visible registry model shipped by
    // nils-agent-session 1.25.10. These deliberately do not reuse the current
    // decoder: deny_unknown_fields must reject every v3-only assignment field.
    #[derive(Debug, Deserialize)]
    #[allow(dead_code)]
    #[serde(deny_unknown_fields)]
    struct ReleasedV2SessionRef {
        #[serde(default)]
        machine: Option<String>,
        session_id: String,
        session_incarnation: String,
        session_created_at: String,
    }

    #[derive(Debug, Deserialize)]
    #[allow(dead_code)]
    #[serde(deny_unknown_fields)]
    struct ReleasedV2RunCheckpoint {
        revision: u64,
        summary: String,
        next_action: String,
        updated_at: String,
    }

    #[derive(Debug, Deserialize)]
    #[allow(dead_code)]
    #[serde(deny_unknown_fields)]
    struct ReleasedV2Run {
        schema_version: String,
        run_id: String,
        revision: u64,
        state: String,
        tier: String,
        objective_summary: String,
        objective_packet_digest: String,
        controller: ReleasedV2SessionRef,
        #[serde(default)]
        durable_refs: Vec<String>,
        #[serde(default)]
        ephemeral: bool,
        #[serde(default)]
        checkpoint: Option<ReleasedV2RunCheckpoint>,
        created_at: String,
        updated_at: String,
    }

    #[derive(Debug, Deserialize)]
    #[allow(dead_code)]
    #[serde(deny_unknown_fields)]
    struct ReleasedV2TimedRelationship {
        session: ReleasedV2SessionRef,
        expires_at: String,
        expires_at_epoch: i64,
    }

    #[derive(Debug, Deserialize)]
    #[allow(dead_code)]
    #[serde(deny_unknown_fields)]
    struct ReleasedV2SubmitRecovery {
        schema_version: String,
        attempt_id: String,
        #[serde(default = "default_submit_recovery_origin")]
        origin: String,
        #[serde(default)]
        run_id: Option<String>,
        #[serde(default)]
        controller: Option<ReleasedV2SessionRef>,
        session_incarnation: String,
        reserved_revision: u64,
        state: String,
        attempt_count: u8,
        result: String,
        attempted_at: String,
        updated_at: String,
    }

    #[derive(Debug, Deserialize)]
    #[allow(dead_code)]
    #[serde(deny_unknown_fields)]
    struct ReleasedV2Assignment {
        schema_version: String,
        assignment_id: String,
        run_id: String,
        revision: u64,
        state: String,
        task_summary: String,
        private_packet_digest: String,
        primary_manager: ReleasedV2SessionRef,
        #[serde(default)]
        worker: Option<ReleasedV2SessionRef>,
        #[serde(default)]
        collaborators: Vec<ReleasedV2SessionRef>,
        #[serde(default)]
        borrowed_by: Vec<ReleasedV2TimedRelationship>,
        #[serde(default)]
        repository: Option<String>,
        #[serde(default)]
        worktree: Option<String>,
        #[serde(default)]
        base_ref: Option<String>,
        #[serde(default)]
        scopes: Vec<String>,
        #[serde(default)]
        durable_refs: Vec<String>,
        #[serde(default)]
        depends_on: Vec<String>,
        #[serde(default)]
        checkpoint: Option<ReleasedV2RunCheckpoint>,
        #[serde(default)]
        result_summary: Option<String>,
        #[serde(default)]
        blocker_summary: Option<String>,
        #[serde(default)]
        submit_recovery: Option<ReleasedV2SubmitRecovery>,
        created_at: String,
        updated_at: String,
    }

    #[derive(Debug, Deserialize)]
    #[allow(dead_code)]
    #[serde(deny_unknown_fields)]
    struct ReleasedV2Receipt {
        principal_session_id: String,
        principal_incarnation: String,
        operation: String,
        request_digest: String,
        outcome: Value,
        created_at_epoch: i64,
    }

    #[derive(Debug, Default, Deserialize)]
    #[serde(default, deny_unknown_fields)]
    struct ReleasedV2Registry {
        schema_version: String,
        runs: BTreeMap<String, ReleasedV2Run>,
        assignments: BTreeMap<String, ReleasedV2Assignment>,
        receipts: BTreeMap<String, ReleasedV2Receipt>,
    }

    #[test]
    fn v2_registry_upgrade_preserves_exact_populated_rollback_for_pinned_reader() {
        let tmp = tempfile::TempDir::new().expect("tempdir");
        let context = CliContext {
            state_dir: tmp.path().to_path_buf(),
            host: Some("test-host".to_string()),
        };
        let root = ensure_orchestration_root(&context).expect("orchestration root");
        let v2_bytes = include_bytes!("../tests/fixtures/orchestration/registry-v2-populated.json");
        let released_before: ReleasedV2Registry =
            serde_json::from_slice(v2_bytes).expect("pinned released-v2 reader fixture");
        assert_eq!(released_before.schema_version, LEGACY_REGISTRY_V2_SCHEMA);
        assert_eq!(released_before.runs.len(), 1);
        assert_eq!(released_before.assignments.len(), 1);
        assert_eq!(released_before.receipts.len(), 1);
        write_atomic(&root.join(REGISTRY_FILE), v2_bytes, SECRET_FILE_MODE).expect("v2 registry");

        let mut locked = lock_registry(&context).expect("lock v2 registry");
        assert_eq!(locked.registry.schema_version, REGISTRY_SCHEMA);
        assert_eq!(
            locked.registry.assignments["assignment-v2"].schema_version,
            ASSIGNMENT_SCHEMA
        );
        let run = locked
            .registry
            .runs
            .get_mut("run-v2")
            .expect("populated run");
        run.revision += 1;
        run.checkpoint = Some(RunCheckpoint {
            revision: run.revision,
            summary: "Representative v3 mutation completed".to_string(),
            next_action: "Exercise explicit rollback".to_string(),
            updated_at: "2030-01-01T00:00:09Z".to_string(),
        });
        run.updated_at = "2030-01-01T00:00:09Z".to_string();
        let assignment = locked
            .registry
            .assignments
            .get_mut("assignment-v2")
            .expect("populated assignment");
        let worker = assignment.worker.clone().expect("released-v2 worker");
        assignment.revision += 1;
        assignment.previous_worker = Some(SessionRef {
            machine: worker.machine.clone(),
            session_id: worker.session_id.clone(),
            session_incarnation: "worker-v2-incarnation-1".to_string(),
            session_created_at: worker.session_created_at.clone(),
        });
        assignment.worker_quarantine = Some(WorkerQuarantineRecord {
            schema_version: WORKER_QUARANTINE_SCHEMA.to_string(),
            worker: worker.clone(),
            reason: "representative v3 cleanup state".to_string(),
            runtime_identity_digest:
                "sha256:3333333333333333333333333333333333333333333333333333333333333333"
                    .to_string(),
            created_at: "2030-01-01T00:00:09Z".to_string(),
        });
        assignment.account_handoff = Some(AccountHandoffReservationRecord {
            schema_version: ACCOUNT_HANDOFF_RESERVATION_SCHEMA.to_string(),
            request_digest: "4444444444444444444444444444444444444444444444444444444444444444"
                .to_string(),
            reservation_id: Some("reservation-v3".to_string()),
            account_intent_id: Some("intent-v3".to_string()),
            run_id: assignment.run_id.clone(),
            controller: assignment.primary_manager.clone(),
            worker,
            reserved_revision: assignment.revision - 1,
            account: "alpha".to_string(),
            created_at: "2030-01-01T00:00:09Z".to_string(),
            updated_at: "2030-01-01T00:00:09Z".to_string(),
        });
        assignment.updated_at = "2030-01-01T00:00:09Z".to_string();
        locked.save().expect("upgrade registry");
        drop(locked);

        let rollback = fs::read(root.join(REGISTRY_V2_ROLLBACK_FILE)).expect("rollback snapshot");
        assert_eq!(
            rollback, v2_bytes,
            "upgrade must preserve the exact pre-migration bytes"
        );
        let upgraded_bytes = fs::read(root.join(REGISTRY_FILE)).expect("upgraded registry bytes");
        assert!(
            serde_json::from_slice::<ReleasedV2Registry>(&upgraded_bytes).is_err(),
            "the pinned released-v2 reader must reject v3 bytes rather than silently misdecode them"
        );
        let upgraded: Registry =
            serde_json::from_slice(&upgraded_bytes).expect("upgraded registry");
        assert_eq!(upgraded.schema_version, REGISTRY_SCHEMA);
        assert_eq!(upgraded.runs["run-v2"].revision, 9);
        assert_eq!(
            upgraded.assignments["assignment-v2"].schema_version,
            ASSIGNMENT_SCHEMA
        );
        assert!(
            upgraded.assignments["assignment-v2"]
                .previous_worker
                .is_some()
        );
        assert!(
            upgraded.assignments["assignment-v2"]
                .worker_quarantine
                .is_some()
        );
        assert!(
            upgraded.assignments["assignment-v2"]
                .account_handoff
                .is_some()
        );

        write_atomic(&root.join(REGISTRY_FILE), &rollback, SECRET_FILE_MODE)
            .expect("operator rollback");
        let restored_for_v2_reader: ReleasedV2Registry =
            serde_json::from_slice(&fs::read(root.join(REGISTRY_FILE)).unwrap())
                .expect("exact pinned released-v2 reader reopens rollback");
        assert_eq!(
            restored_for_v2_reader.schema_version, LEGACY_REGISTRY_V2_SCHEMA,
            "the rollback snapshot remains consumable by the released v2 reader"
        );
        let restored_assignment = &restored_for_v2_reader.assignments["assignment-v2"];
        assert_eq!(
            restored_assignment.schema_version,
            LEGACY_ASSIGNMENT_V2_SCHEMA
        );
        assert!(restored_assignment.submit_recovery.is_some());
        let restored_receipt =
            &restored_for_v2_reader.receipts["main-v2:main-v2-incarnation:receipt-v2"];
        assert_eq!(restored_receipt.outcome["guidance"]["unread_count"], 1);
        assert_eq!(restored_receipt.outcome["account_reservation"], "intent-v2");
        assert_eq!(restored_receipt.outcome["cleanup_state"], "quarantined");
        assert_eq!(
            restored_for_v2_reader.runs["run-v2"]
                .checkpoint
                .as_ref()
                .map(|checkpoint| checkpoint.revision),
            Some(8),
            "rollback intentionally restores the exact pre-v3 mutation boundary"
        );
        assert_eq!(
            load_registry_readonly(&context)
                .expect("current reader reopens restored state")
                .schema_version,
            REGISTRY_SCHEMA
        );
    }

    #[test]
    fn rollback_durability_failure_never_replaces_the_live_v2_registry() {
        let tmp = tempfile::TempDir::new().expect("tempdir");
        let context = CliContext {
            state_dir: tmp.path().to_path_buf(),
            host: Some("test-host".to_string()),
        };
        let root = ensure_orchestration_root(&context).expect("orchestration root");
        let v2_bytes = include_bytes!("../tests/fixtures/orchestration/registry-v2-populated.json");
        write_atomic(&root.join(REGISTRY_FILE), v2_bytes, SECRET_FILE_MODE).expect("v2 registry");

        let mut locked = lock_registry(&context).expect("lock v2 registry");
        let error = locked
            .save_with_rollback_durability(|_, _| Err(store_unavailable()))
            .expect_err("failed rollback directory sync must abort migration");
        assert_eq!(error.code(), "orchestration-store-unavailable");
        assert_eq!(
            fs::read(root.join(REGISTRY_FILE)).expect("live registry"),
            v2_bytes,
            "v3 bytes must not replace the live registry before strict rollback durability"
        );
    }

    #[test]
    fn registry_reader_rejects_symlink_fifo_and_oversize_inputs_without_blocking() {
        use std::ffi::CString;
        use std::os::unix::ffi::OsStrExt;
        use std::os::unix::fs::symlink;

        let tmp = tempfile::TempDir::new().expect("tempdir");
        let registry = tmp.path().join(REGISTRY_FILE);
        let outside = tmp.path().join("outside.json");
        write_atomic(&outside, b"{}", SECRET_FILE_MODE).expect("outside registry fixture");
        symlink(&outside, &registry).expect("registry symlink fixture");
        let symlink_error =
            read_registry_bytes(&registry).expect_err("registry reads must fail on a symlink");
        assert_eq!(
            symlink_error.code(),
            "orchestration-store-invalid",
            "an unsafe pathname is persistent invalid state, not retryable unavailability"
        );

        fs::remove_file(&registry).expect("remove registry symlink");
        let registry_c = CString::new(registry.as_os_str().as_bytes()).expect("registry path");
        assert_eq!(
            unsafe { libc::mkfifo(registry_c.as_ptr(), SECRET_FILE_MODE) },
            0,
            "create registry fifo fixture"
        );
        let started = Instant::now();
        assert!(
            read_registry_bytes(&registry).is_err(),
            "registry reads must reject non-regular descriptors"
        );
        assert!(
            started.elapsed() < Duration::from_millis(500),
            "the nonblocking descriptor open must not wait for a FIFO writer"
        );

        fs::remove_file(&registry).expect("remove registry fifo");
        let oversized = OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(SECRET_FILE_MODE)
            .open(&registry)
            .expect("oversized registry");
        oversized
            .set_len(MAX_REGISTRY_BYTES + 1)
            .expect("sparse oversized registry");
        let error = read_registry_bytes(&registry)
            .expect_err("oversized registry must fail before its body is read");
        assert_eq!(error.code(), "orchestration-store-invalid");
    }

    #[test]
    fn existing_rollback_symlink_and_fifo_fail_without_replacing_live_registry() {
        use std::ffi::CString;
        use std::os::unix::ffi::OsStrExt;
        use std::os::unix::fs::symlink;

        for special in ["symlink", "fifo"] {
            let tmp = tempfile::TempDir::new().expect("tempdir");
            let context = CliContext {
                state_dir: tmp.path().to_path_buf(),
                host: Some("test-host".to_string()),
            };
            let root = ensure_orchestration_root(&context).expect("orchestration root");
            let v2_bytes =
                include_bytes!("../tests/fixtures/orchestration/registry-v2-populated.json");
            write_atomic(&root.join(REGISTRY_FILE), v2_bytes, SECRET_FILE_MODE)
                .expect("v2 registry");
            let mut locked = lock_registry(&context).expect("lock v2 registry");
            let rollback = root.join(REGISTRY_V2_ROLLBACK_FILE);
            match special {
                "symlink" => {
                    let outside = tmp.path().join("outside-rollback.json");
                    write_atomic(&outside, v2_bytes, SECRET_FILE_MODE)
                        .expect("outside rollback fixture");
                    symlink(outside, &rollback).expect("rollback symlink fixture");
                }
                "fifo" => {
                    let rollback_c =
                        CString::new(rollback.as_os_str().as_bytes()).expect("rollback path");
                    assert_eq!(
                        unsafe { libc::mkfifo(rollback_c.as_ptr(), SECRET_FILE_MODE) },
                        0,
                        "create rollback fifo fixture"
                    );
                }
                _ => unreachable!(),
            }

            let started = Instant::now();
            let error = locked
                .save()
                .expect_err("special rollback descriptor must fail closed");
            assert_eq!(
                error.code(),
                "orchestration-store-invalid",
                "{special} rollback state must not be classified as transient"
            );
            assert!(
                started.elapsed() < Duration::from_millis(500),
                "{special} rollback verification must remain bounded"
            );
            assert_eq!(
                fs::read(root.join(REGISTRY_FILE)).expect("live registry"),
                v2_bytes,
                "{special} rollback failure must leave the live v2 bytes unchanged"
            );
        }
    }

    #[test]
    fn rollback_durability_rejects_path_replacement_after_descriptor_validation() {
        use std::ffi::CString;
        use std::os::unix::ffi::OsStrExt;

        for replacement in ["regular", "fifo"] {
            let tmp = tempfile::TempDir::new().expect("tempdir");
            fs::set_permissions(tmp.path(), fs::Permissions::from_mode(0o700))
                .expect("private rollback parent");
            let rollback = tmp.path().join(REGISTRY_V2_ROLLBACK_FILE);
            write_atomic(&rollback, b"released-v2", SECRET_FILE_MODE).expect("rollback fixture");
            let snapshot = read_private_bounded_file(
                &rollback,
                "orchestration rollback snapshot exceeds byte limit",
            )
            .expect("read rollback")
            .expect("rollback exists");
            fs::remove_file(&rollback).expect("unlink validated rollback path");
            match replacement {
                "regular" => {
                    write_atomic(&rollback, b"different", SECRET_FILE_MODE)
                        .expect("replacement rollback");
                }
                "fifo" => {
                    let rollback_c =
                        CString::new(rollback.as_os_str().as_bytes()).expect("rollback path");
                    assert_eq!(
                        unsafe { libc::mkfifo(rollback_c.as_ptr(), SECRET_FILE_MODE) },
                        0,
                        "replacement rollback fifo"
                    );
                }
                _ => unreachable!(),
            }

            let started = Instant::now();
            sync_rollback_snapshot(&snapshot, &rollback)
                .expect_err("path replacement must fail before live registry replacement");
            assert!(
                started.elapsed() < Duration::from_millis(500),
                "{replacement} replacement detection must remain bounded"
            );
        }
    }

    #[test]
    fn oversized_existing_v1_and_v2_rollback_snapshots_fail_before_body_read() {
        for (schema, rollback_file) in [
            (LEGACY_REGISTRY_V1_SCHEMA, REGISTRY_V1_ROLLBACK_FILE),
            (LEGACY_REGISTRY_V2_SCHEMA, REGISTRY_V2_ROLLBACK_FILE),
        ] {
            let tmp = tempfile::TempDir::new().expect("tempdir");
            let context = CliContext {
                state_dir: tmp.path().to_path_buf(),
                host: Some("test-host".to_string()),
            };
            let root = ensure_orchestration_root(&context).expect("orchestration root");
            let mut released = Registry::empty();
            released.schema_version = schema.to_string();
            let released_bytes = serde_json::to_vec_pretty(&released).expect("released registry");
            write_atomic(&root.join(REGISTRY_FILE), &released_bytes, SECRET_FILE_MODE)
                .expect("released registry");
            let oversized = OpenOptions::new()
                .write(true)
                .create_new(true)
                .mode(SECRET_FILE_MODE)
                .open(root.join(rollback_file))
                .expect("oversized rollback");
            oversized
                .set_len(MAX_REGISTRY_BYTES + 1)
                .expect("sparse oversized rollback");

            let mut locked = lock_registry(&context).expect("lock released registry");
            let error = locked
                .save()
                .expect_err("oversized rollback snapshot must fail closed");
            assert_eq!(error.code(), "orchestration-store-invalid");
            assert_eq!(
                fs::read(root.join(REGISTRY_FILE)).expect("live registry"),
                released_bytes,
                "{schema} live registry must remain unchanged"
            );
        }
    }

    #[test]
    fn unrelated_session_quarantine_check_does_not_load_the_orchestration_registry() {
        let tmp = tempfile::TempDir::new().expect("tempdir");
        let context = CliContext {
            state_dir: tmp.path().to_path_buf(),
            host: Some("test-host".to_string()),
        };
        fs::create_dir_all(orchestration_root(&context)).expect("orchestration dir");
        fs::write(
            orchestration_root(&context).join(REGISTRY_FILE),
            b"not-json",
        )
        .expect("corrupt registry");
        let record: SessionRecord = serde_json::from_value(json!({
            "schema_version": "agent-session.session.v1",
            "id": "unrelated-session",
            "agent": "codex",
            "mode": "interactive",
            "title": null,
            "cwd": tmp.path(),
            "tmux_session": "unrelated-session",
            "prompt_file": null,
            "log_file": null,
            "created_at": "2030-01-01T00:00:00Z",
            "updated_at": "2030-01-01T00:00:00Z"
        }))
        .expect("session record");

        ensure_session_not_quarantined(&context, &record)
            .expect("an unrelated session has no quarantine marker");
    }

    #[test]
    fn registry_rejects_unknown_run_state_without_leaking_record_content() {
        let mut registry = Registry::empty();
        registry.runs.insert(
            "run-one".to_string(),
            RunRecord {
                schema_version: RUN_SCHEMA.to_string(),
                run_id: "run-one".to_string(),
                revision: 1,
                state: "future".to_string(),
                tier: "L0".to_string(),
                objective_summary: "safe summary".to_string(),
                objective_packet_digest: format!("sha256:{}", "a".repeat(64)),
                controller: SessionRef {
                    machine: None,
                    session_id: "main".to_string(),
                    session_incarnation: "incarnation".to_string(),
                    session_created_at: "2030-01-01T00:00:00Z".to_string(),
                },
                durable_refs: Vec::new(),
                ephemeral: false,
                checkpoint: None,
                created_at: "2030-01-01T00:00:00Z".to_string(),
                updated_at: "2030-01-01T00:00:00Z".to_string(),
            },
        );
        let error = registry
            .validate()
            .expect_err("future state must fail closed");
        assert_eq!(error.code(), "orchestration-store-invalid");
        assert!(!error.message().contains("safe summary"));
    }
}

use std::collections::{BTreeMap, HashSet};
use std::fs;
use std::io::{BufRead, BufReader, Seek, SeekFrom};
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::{Duration, Instant, SystemTime};

use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};

const SCAN_MAX_ENTRIES: usize = 10_000;
const SCAN_MAX_DURATION: Duration = Duration::from_secs(2);
const MAX_LINE_BYTES: usize = 1024 * 1024;
const MAX_ARCHIVE_BYTES: u64 = 64 * 1024;
const MAX_PREVIEW_LINES: usize = 128;
const MAX_PREVIEW_CHARS: usize = 240;
const MAX_MESSAGE_CHARS: usize = 64 * 1024;
const MAX_MESSAGE_SCAN_LINES: usize = 10_000;
const MESSAGE_SCAN_MAX_DURATION: Duration = Duration::from_secs(2);
const CATALOG_TTL: Duration = Duration::from_secs(30);

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct HistorySource {
    pub(crate) provider: String,
    pub(crate) agent_profile: Option<String>,
    pub(crate) root: PathBuf,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub(crate) struct ArchivedSession {
    pub(crate) schema_version: String,
    pub(crate) history_id: String,
    pub(crate) provider: String,
    pub(crate) provider_session_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) agent_profile: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) title: Option<String>,
    pub(crate) cwd: String,
    pub(crate) created_at: String,
    pub(crate) updated_at: String,
    pub(crate) archived_at: String,
}

#[derive(Clone, Debug, Serialize)]
pub(crate) struct HistorySession {
    pub(crate) id: String,
    pub(crate) provider: String,
    pub(crate) provider_session_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) agent_profile: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) title: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) prompt_preview: Option<String>,
    pub(crate) cwd: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) repo_name: Option<String>,
    pub(crate) created_at: String,
    pub(crate) updated_at: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) archived_at: Option<String>,
    pub(crate) resumable: bool,
    #[serde(skip)]
    transcript_path: PathBuf,
}

#[derive(Debug, Serialize)]
pub(crate) struct HistoryPage {
    pub(crate) sessions: Vec<HistorySession>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) next_cursor: Option<String>,
    pub(crate) truncated: bool,
}

#[derive(Debug, Serialize)]
pub(crate) struct HistoryMessage {
    pub(crate) id: String,
    pub(crate) role: String,
    pub(crate) text: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) timestamp: Option<String>,
}

#[derive(Debug, Serialize)]
pub(crate) struct HistoryMessagesPage {
    pub(crate) messages: Vec<HistoryMessage>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) next_cursor: Option<String>,
}

#[derive(Debug)]
pub(crate) enum HistoryError {
    NotFound,
    InvalidCursor,
    Io,
}

#[derive(Debug)]
pub(crate) struct PendingArchive {
    archive: ArchivedSession,
    path: PathBuf,
    backup: Option<PathBuf>,
    committed: bool,
}

impl PendingArchive {
    pub(crate) fn commit(mut self) -> ArchivedSession {
        if let Some(backup) = self.backup.take() {
            let _ = fs::remove_file(backup);
        }
        self.committed = true;
        self.archive.clone()
    }

    fn rollback(&mut self) {
        if self.committed {
            return;
        }
        let _ = fs::remove_file(&self.path);
        if let Some(backup) = self.backup.take() {
            let _ = fs::rename(backup, &self.path);
        }
        self.committed = true;
    }
}

impl Drop for PendingArchive {
    fn drop(&mut self) {
        self.rollback();
    }
}

#[derive(Clone, Debug)]
struct CatalogSnapshot {
    sessions: Vec<HistorySession>,
    truncated: bool,
}

#[derive(Debug, Default)]
struct CatalogCache {
    refreshed_at: Option<Instant>,
    snapshot: Option<CatalogSnapshot>,
}

#[derive(Debug)]
pub(crate) struct HistoryCatalog {
    sources: Vec<HistorySource>,
    archives_root: PathBuf,
    cache: Mutex<CatalogCache>,
}

impl HistoryCatalog {
    pub(crate) fn new(sources: Vec<HistorySource>, archives_root: PathBuf) -> Self {
        Self {
            sources,
            archives_root,
            cache: Mutex::new(CatalogCache::default()),
        }
    }

    pub(crate) fn invalidate(&self) {
        let mut cache = self
            .cache
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        cache.refreshed_at = None;
    }

    pub(crate) fn list(
        &self,
        machine: &str,
        query: Option<&str>,
        provider: Option<&str>,
        cursor: Option<&str>,
        limit: usize,
    ) -> Result<HistoryPage, HistoryError> {
        paginate_snapshot(self.snapshot(), machine, query, provider, cursor, limit)
    }

    pub(crate) fn messages(
        &self,
        history_id: &str,
        cursor: Option<&str>,
        limit: usize,
    ) -> Result<HistoryMessagesPage, HistoryError> {
        let offset = parse_message_cursor(cursor)?;
        let snapshot = self.snapshot();
        let session = snapshot
            .sessions
            .iter()
            .find(|session| session.id == history_id)
            .ok_or(HistoryError::NotFound)?;
        read_messages(session, offset, limit.clamp(1, 100))
    }

    fn snapshot(&self) -> CatalogSnapshot {
        let mut cache = self
            .cache
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let fresh = cache
            .refreshed_at
            .is_some_and(|refreshed_at| refreshed_at.elapsed() < CATALOG_TTL);
        if fresh && let Some(snapshot) = cache.snapshot.clone() {
            return snapshot;
        }
        let snapshot = scan_catalog(&self.sources, &self.archives_root);
        cache.refreshed_at = Some(Instant::now());
        cache.snapshot = Some(snapshot.clone());
        snapshot
    }
}

pub(crate) fn stable_history_id(
    provider: &str,
    agent_profile: Option<&str>,
    provider_session_id: &str,
) -> String {
    let mut digest = Sha256::new();
    digest.update(provider.as_bytes());
    digest.update([0]);
    digest.update(agent_profile.unwrap_or_default().as_bytes());
    digest.update([0]);
    digest.update(provider_session_id.as_bytes());
    let encoded = digest
        .finalize()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect::<String>();
    format!("h-{encoded}")
}

pub(crate) fn archive_root(state_dir: &Path) -> PathBuf {
    state_dir.join("history").join("archives")
}

pub(crate) fn write_archive(
    root: &Path,
    archive: &ArchivedSession,
) -> Result<PendingArchive, HistoryError> {
    fs::create_dir_all(root).map_err(|_| HistoryError::Io)?;
    if fs::symlink_metadata(root)
        .map_err(|_| HistoryError::Io)?
        .file_type()
        .is_symlink()
    {
        return Err(HistoryError::Io);
    }
    fs::set_permissions(root, fs::Permissions::from_mode(0o700)).map_err(|_| HistoryError::Io)?;
    let path = root.join(format!("{}.json", archive.history_id));
    let temp = root.join(format!(
        ".{}.tmp-{}",
        archive.history_id,
        uuid::Uuid::new_v4()
    ));
    let backup = root.join(format!(
        ".{}.backup-{}",
        archive.history_id,
        uuid::Uuid::new_v4()
    ));
    let bytes = serde_json::to_vec_pretty(archive).map_err(|_| HistoryError::Io)?;
    let result = (|| {
        let mut options = fs::OpenOptions::new();
        options.write(true).create_new(true).mode(0o600);
        let mut file = options.open(&temp).map_err(|_| HistoryError::Io)?;
        std::io::Write::write_all(&mut file, &bytes).map_err(|_| HistoryError::Io)?;
        file.sync_all().map_err(|_| HistoryError::Io)?;
        let previous = match fs::symlink_metadata(&path) {
            Ok(metadata) if metadata.file_type().is_symlink() || !metadata.is_file() => {
                return Err(HistoryError::Io);
            }
            Ok(_) => {
                fs::rename(&path, &backup).map_err(|_| HistoryError::Io)?;
                Some(backup.clone())
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
            Err(_) => return Err(HistoryError::Io),
        };
        if fs::rename(&temp, &path).is_err() {
            if let Some(previous) = previous {
                let _ = fs::rename(previous, &path);
            }
            return Err(HistoryError::Io);
        }
        Ok(previous)
    })();
    let previous = match result {
        Ok(previous) => previous,
        Err(error) => {
            let _ = fs::remove_file(&temp);
            return Err(error);
        }
    };
    Ok(PendingArchive {
        archive: archive.clone(),
        path,
        backup: previous,
        committed: false,
    })
}

#[cfg(test)]
pub(crate) fn list(
    sources: &[HistorySource],
    archives_root: &Path,
    machine: &str,
    query: Option<&str>,
    provider: Option<&str>,
    cursor: Option<&str>,
    limit: usize,
) -> Result<HistoryPage, HistoryError> {
    paginate_snapshot(
        scan_catalog(sources, archives_root),
        machine,
        query,
        provider,
        cursor,
        limit,
    )
}

fn paginate_snapshot(
    snapshot: CatalogSnapshot,
    machine: &str,
    query: Option<&str>,
    provider: Option<&str>,
    cursor: Option<&str>,
    limit: usize,
) -> Result<HistoryPage, HistoryError> {
    let cursor = cursor.map(parse_list_cursor).transpose()?;
    let limit = limit.clamp(1, 100);
    let mut sessions = snapshot.sessions;
    sessions.retain(|session| {
        provider.is_none_or(|value| value == session.provider)
            && matches_metadata(session, machine, query)
    });
    sessions.sort_by(|left, right| {
        right
            .updated_at
            .cmp(&left.updated_at)
            .then_with(|| right.created_at.cmp(&left.created_at))
            .then_with(|| left.id.cmp(&right.id))
    });
    if let Some(cursor) = cursor {
        sessions.retain(|session| session_is_after_cursor(session, &cursor));
    }
    let has_more = sessions.len() > limit;
    let page = sessions.into_iter().take(limit).collect::<Vec<_>>();
    let next_cursor = has_more
        .then(|| page.last().map(encode_list_cursor))
        .flatten();
    Ok(HistoryPage {
        sessions: page,
        next_cursor,
        truncated: snapshot.truncated,
    })
}

#[derive(Debug)]
struct ListCursor {
    updated_at: String,
    created_at: String,
    id: String,
}

fn encode_list_cursor(session: &HistorySession) -> String {
    format!(
        "v1|{}|{}|{}",
        session.updated_at, session.created_at, session.id
    )
}

fn parse_list_cursor(value: &str) -> Result<ListCursor, HistoryError> {
    let mut parts = value.split('|');
    let cursor = ListCursor {
        updated_at: match parts.next() {
            Some("v1") => parts.next().unwrap_or_default().to_string(),
            _ => return Err(HistoryError::InvalidCursor),
        },
        created_at: parts.next().unwrap_or_default().to_string(),
        id: parts.next().unwrap_or_default().to_string(),
    };
    if cursor.updated_at.is_empty()
        || cursor.created_at.is_empty()
        || cursor.id.is_empty()
        || parts.next().is_some()
    {
        return Err(HistoryError::InvalidCursor);
    }
    Ok(cursor)
}

fn session_is_after_cursor(session: &HistorySession, cursor: &ListCursor) -> bool {
    session.updated_at < cursor.updated_at
        || (session.updated_at == cursor.updated_at
            && (session.created_at < cursor.created_at
                || (session.created_at == cursor.created_at && session.id > cursor.id)))
}

fn scan_catalog(sources: &[HistorySource], archives_root: &Path) -> CatalogSnapshot {
    let deadline = Instant::now() + SCAN_MAX_DURATION;
    let mut truncated = false;
    let archives = read_archives(archives_root, deadline, &mut truncated);
    let mut visited = 0usize;
    let mut sessions = Vec::new();
    let mut seen = HashSet::new();

    for source in sources {
        let mut paths = Vec::new();
        collect_jsonl(
            &source.root,
            if source.provider == "codex" { 6 } else { 2 },
            0,
            &mut paths,
            &mut visited,
            deadline,
            &mut truncated,
        );
        for path in paths {
            if Instant::now() >= deadline {
                truncated = true;
                break;
            }
            let Some(mut session) = inspect_history_file(source, &path, deadline) else {
                continue;
            };
            let archive = archives.get(&session.id);
            if let Some(archive) = archive {
                session.id = archive.history_id.clone();
                session.agent_profile = archive.agent_profile.clone();
                session.title = archive.title.clone();
                session.cwd = archive.cwd.clone();
                session.repo_name = repo_name(&archive.cwd);
                session.archived_at = Some(archive.archived_at.clone());
            }
            if !seen.insert(session.id.clone()) {
                continue;
            }
            sessions.push(session);
            if Instant::now() >= deadline {
                truncated = true;
                break;
            }
        }
        if truncated {
            break;
        }
    }

    for archive in archives.values() {
        if seen.insert(archive.history_id.clone()) {
            sessions.push(HistorySession {
                id: archive.history_id.clone(),
                provider: archive.provider.clone(),
                provider_session_id: archive.provider_session_id.clone(),
                agent_profile: archive.agent_profile.clone(),
                title: archive.title.clone(),
                prompt_preview: None,
                cwd: archive.cwd.clone(),
                repo_name: repo_name(&archive.cwd),
                created_at: archive.created_at.clone(),
                updated_at: archive.updated_at.clone(),
                archived_at: Some(archive.archived_at.clone()),
                resumable: false,
                transcript_path: PathBuf::new(),
            });
        }
    }

    CatalogSnapshot {
        sessions,
        truncated,
    }
}

#[cfg(test)]
pub(crate) fn messages(
    sources: &[HistorySource],
    history_id: &str,
    cursor: Option<&str>,
    limit: usize,
) -> Result<HistoryMessagesPage, HistoryError> {
    let offset = parse_message_cursor(cursor)?;
    let limit = limit.clamp(1, 100);
    let session = find_session(sources, history_id).ok_or(HistoryError::NotFound)?;
    read_messages(&session, offset, limit)
}

fn parse_message_cursor(cursor: Option<&str>) -> Result<u64, HistoryError> {
    cursor
        .map(str::parse::<u64>)
        .transpose()
        .map_err(|_| HistoryError::InvalidCursor)
        .map(|cursor| cursor.unwrap_or(0))
}

#[cfg(test)]
fn find_session(sources: &[HistorySource], history_id: &str) -> Option<HistorySession> {
    let deadline = Instant::now() + SCAN_MAX_DURATION;
    let mut visited = 0;
    let mut truncated = false;
    for source in sources {
        let mut paths = Vec::new();
        collect_jsonl(
            &source.root,
            if source.provider == "codex" { 6 } else { 2 },
            0,
            &mut paths,
            &mut visited,
            deadline,
            &mut truncated,
        );
        for path in paths {
            if Instant::now() >= deadline {
                truncated = true;
                break;
            }
            let Some(session) = inspect_history_file(source, &path, deadline) else {
                continue;
            };
            if session.id == history_id {
                return Some(session);
            }
        }
        if truncated {
            break;
        }
    }
    None
}

fn collect_jsonl(
    dir: &Path,
    max_depth: usize,
    depth: usize,
    output: &mut Vec<PathBuf>,
    visited: &mut usize,
    deadline: Instant,
    truncated: &mut bool,
) {
    if depth > max_depth || *truncated {
        return;
    }
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        if *visited >= SCAN_MAX_ENTRIES || Instant::now() >= deadline {
            *truncated = true;
            return;
        }
        *visited += 1;
        let Ok(kind) = entry.file_type() else {
            continue;
        };
        if kind.is_dir() {
            collect_jsonl(
                &entry.path(),
                max_depth,
                depth + 1,
                output,
                visited,
                deadline,
                truncated,
            );
        } else if kind.is_file()
            && entry.path().extension().and_then(|value| value.to_str()) == Some("jsonl")
        {
            output.push(entry.path());
        }
    }
}

fn inspect_history_file(
    source: &HistorySource,
    path: &Path,
    deadline: Instant,
) -> Option<HistorySession> {
    if Instant::now() >= deadline {
        return None;
    }
    let metadata = fs::metadata(path).ok()?;
    let updated_at = format_system_time(metadata.modified().ok()?);
    let (provider_session_id, cwd, created_at) = match source.provider.as_str() {
        "codex" => {
            let meta = nils_provider_resume::read_codex_resumable_session_meta(path)?;
            (
                meta.session_id,
                meta.cwd,
                format_system_time(meta.created_at),
            )
        }
        "claude" => {
            let id = path.file_stem()?.to_str()?.to_string();
            let cwd = nils_provider_resume::read_claude_session_cwd(path, &id)?;
            (id, cwd, updated_at.clone())
        }
        _ => return None,
    };
    let id = stable_history_id(
        &source.provider,
        source.agent_profile.as_deref(),
        &provider_session_id,
    );
    Some(HistorySession {
        id,
        provider: source.provider.clone(),
        provider_session_id,
        agent_profile: source.agent_profile.clone(),
        title: None,
        prompt_preview: first_prompt(path, &source.provider, deadline),
        repo_name: repo_name(&cwd),
        cwd,
        created_at,
        updated_at,
        archived_at: None,
        resumable: true,
        transcript_path: path.to_path_buf(),
    })
}

fn first_prompt(path: &Path, provider: &str, deadline: Instant) -> Option<String> {
    let file = fs::File::open(path).ok()?;
    let mut reader = BufReader::new(file);
    let mut line = Vec::new();
    for _ in 0..MAX_PREVIEW_LINES {
        if Instant::now() >= deadline {
            return None;
        }
        let bounded = read_bounded_line(&mut reader, &mut line).ok()?;
        if bounded == BoundedLine::Eof {
            return None;
        }
        if bounded == BoundedLine::Oversized {
            continue;
        }
        let Ok(value) = serde_json::from_slice::<Value>(&line) else {
            continue;
        };
        let Some((role, text, _)) = normalize_message(provider, &value) else {
            continue;
        };
        if role == "user" && !text.trim().is_empty() {
            return Some(truncate_chars(text.trim(), MAX_PREVIEW_CHARS));
        }
    }
    None
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum BoundedLine {
    Eof,
    Line,
    Oversized,
}

fn read_bounded_line<R: BufRead>(
    reader: &mut R,
    output: &mut Vec<u8>,
) -> std::io::Result<BoundedLine> {
    output.clear();
    let mut read_any = false;
    let mut oversized = false;
    loop {
        let available = reader.fill_buf()?;
        if available.is_empty() {
            return Ok(if !read_any {
                BoundedLine::Eof
            } else if oversized {
                BoundedLine::Oversized
            } else {
                BoundedLine::Line
            });
        }
        let end = available
            .iter()
            .position(|byte| *byte == b'\n')
            .map_or(available.len(), |position| position + 1);
        let ends_line = available.get(end.saturating_sub(1)) == Some(&b'\n');
        read_any = true;
        if !oversized {
            let remaining = MAX_LINE_BYTES.saturating_sub(output.len());
            if end <= remaining {
                output.extend_from_slice(&available[..end]);
            } else {
                output.extend_from_slice(&available[..remaining]);
                oversized = true;
            }
        }
        reader.consume(end);
        if ends_line {
            return Ok(if oversized {
                BoundedLine::Oversized
            } else {
                BoundedLine::Line
            });
        }
    }
}

fn read_messages(
    session: &HistorySession,
    offset: u64,
    limit: usize,
) -> Result<HistoryMessagesPage, HistoryError> {
    let file = fs::File::open(&session.transcript_path).map_err(|_| HistoryError::NotFound)?;
    let mut reader = BufReader::new(file);
    reader
        .seek(SeekFrom::Start(offset))
        .map_err(|_| HistoryError::InvalidCursor)?;
    let mut messages = Vec::new();
    let mut line = Vec::new();
    let mut ordinal = 0usize;
    let mut scanned = 0usize;
    let deadline = Instant::now() + MESSAGE_SCAN_MAX_DURATION;
    while messages.len() < limit {
        if scanned >= MAX_MESSAGE_SCAN_LINES || Instant::now() >= deadline {
            let next = reader.stream_position().map_err(|_| HistoryError::Io)?;
            return Ok(HistoryMessagesPage {
                messages,
                next_cursor: Some(next.to_string()),
            });
        }
        line.clear();
        let bounded = read_bounded_line(&mut reader, &mut line).map_err(|_| HistoryError::Io)?;
        if bounded == BoundedLine::Eof {
            return Ok(HistoryMessagesPage {
                messages,
                next_cursor: None,
            });
        }
        scanned += 1;
        if bounded == BoundedLine::Oversized {
            continue;
        }
        let Ok(value) = serde_json::from_slice::<Value>(&line) else {
            continue;
        };
        let Some((role, text, timestamp)) = normalize_message(&session.provider, &value) else {
            continue;
        };
        if text.trim().is_empty() {
            continue;
        }
        ordinal += 1;
        messages.push(HistoryMessage {
            id: format!("{}-{offset}-{ordinal}", session.id),
            role,
            text: truncate_chars(text.trim(), MAX_MESSAGE_CHARS),
            timestamp,
        });
    }
    let next = reader.stream_position().map_err(|_| HistoryError::Io)?;
    Ok(HistoryMessagesPage {
        messages,
        next_cursor: Some(next.to_string()),
    })
}

fn normalize_message(provider: &str, value: &Value) -> Option<(String, String, Option<String>)> {
    let timestamp = value
        .get("timestamp")
        .and_then(Value::as_str)
        .map(str::to_string);
    match provider {
        "codex" => {
            if value.get("type").and_then(Value::as_str) != Some("response_item") {
                return None;
            }
            let payload = value.get("payload")?;
            if payload.get("type").and_then(Value::as_str) != Some("message") {
                return None;
            }
            let role = payload.get("role")?.as_str()?;
            if !matches!(role, "user" | "assistant") {
                return None;
            }
            let text = content_text(payload.get("content")?);
            Some((role.to_string(), text, timestamp))
        }
        "claude" => {
            let role = value.get("type")?.as_str()?;
            if !matches!(role, "user" | "assistant")
                || value.get("isSidechain").and_then(Value::as_bool) == Some(true)
            {
                return None;
            }
            let message = value.get("message")?;
            Some((
                role.to_string(),
                content_text(message.get("content")?),
                timestamp,
            ))
        }
        _ => None,
    }
}

fn content_text(content: &Value) -> String {
    if let Some(text) = content.as_str() {
        return text.to_string();
    }
    content
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|part| {
            part.get("text")
                .and_then(Value::as_str)
                .or_else(|| part.get("content").and_then(Value::as_str))
        })
        .collect::<Vec<_>>()
        .join("\n")
}

fn matches_metadata(session: &HistorySession, machine: &str, query: Option<&str>) -> bool {
    let Some(query) = query.map(str::trim).filter(|value| !value.is_empty()) else {
        return true;
    };
    let query = query.to_lowercase();
    [
        session.title.as_deref(),
        session.prompt_preview.as_deref(),
        Some(session.provider_session_id.as_str()),
        Some(session.provider.as_str()),
        session.agent_profile.as_deref(),
        Some(session.cwd.as_str()),
        session.repo_name.as_deref(),
        Some(machine),
        Some(session.created_at.as_str()),
        Some(session.updated_at.as_str()),
        session.archived_at.as_deref(),
    ]
    .into_iter()
    .flatten()
    .any(|value| value.to_lowercase().contains(&query))
}

fn read_archives(
    root: &Path,
    deadline: Instant,
    truncated: &mut bool,
) -> BTreeMap<String, ArchivedSession> {
    let mut archives = BTreeMap::new();
    let Ok(entries) = fs::read_dir(root) else {
        return archives;
    };
    for entry in entries.flatten().take(SCAN_MAX_ENTRIES) {
        if Instant::now() >= deadline {
            *truncated = true;
            break;
        }
        let path = entry.path();
        if path.extension().and_then(|value| value.to_str()) != Some("json") {
            continue;
        }
        let Ok(metadata) = fs::metadata(&path) else {
            continue;
        };
        if metadata.len() > MAX_ARCHIVE_BYTES {
            continue;
        }
        let Ok(bytes) = fs::read(path) else { continue };
        let Ok(archive) = serde_json::from_slice::<ArchivedSession>(&bytes) else {
            continue;
        };
        archives.insert(archive.history_id.clone(), archive);
    }
    archives
}

fn format_system_time(value: SystemTime) -> String {
    jiff::Timestamp::try_from(value)
        .map(|timestamp| timestamp.to_string())
        .unwrap_or_default()
}

fn repo_name(cwd: &str) -> Option<String> {
    Path::new(cwd)
        .file_name()
        .and_then(|value| value.to_str())
        .filter(|value| !value.is_empty())
        .map(str::to_string)
}

fn truncate_chars(value: &str, max: usize) -> String {
    if value.chars().count() <= max {
        return value.to_string();
    }
    let mut output: String = value.chars().take(max.saturating_sub(1)).collect();
    output.push('…');
    output
}

#[cfg(test)]
mod tests {
    use super::*;

    fn history_session(id: &str, updated_at: &str) -> HistorySession {
        HistorySession {
            id: id.to_string(),
            provider: "codex".to_string(),
            provider_session_id: id.to_string(),
            agent_profile: None,
            title: None,
            prompt_preview: None,
            cwd: "/work/example".to_string(),
            repo_name: Some("example".to_string()),
            created_at: updated_at.to_string(),
            updated_at: updated_at.to_string(),
            archived_at: None,
            resumable: true,
            transcript_path: PathBuf::new(),
        }
    }

    #[test]
    fn metadata_search_does_not_match_non_preview_transcript_text() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("sessions/2026/08/31");
        fs::create_dir_all(&root).unwrap();
        fs::write(
            root.join("rollout.jsonl"),
            concat!(
                "{\"timestamp\":\"2026-08-31T00:00:00Z\",\"type\":\"session_meta\",\"payload\":{\"id\":\"abc\",\"cwd\":\"/work/example\",\"source\":\"cli\",\"timestamp\":\"2026-08-31T00:00:00Z\"}}\n",
                "{\"timestamp\":\"2026-08-31T00:00:01Z\",\"type\":\"response_item\",\"payload\":{\"type\":\"message\",\"role\":\"user\",\"content\":[{\"type\":\"input_text\",\"text\":\"visible preview\"}]}}\n",
                "{\"timestamp\":\"2026-08-31T00:00:02Z\",\"type\":\"response_item\",\"payload\":{\"type\":\"message\",\"role\":\"assistant\",\"content\":[{\"type\":\"output_text\",\"text\":\"secret needle\"}]}}\n"
            ),
        )
        .unwrap();
        let sources = [HistorySource {
            provider: "codex".into(),
            agent_profile: None,
            root: tmp.path().join("sessions"),
        }];

        let preview = list(
            &sources,
            &tmp.path().join("archives"),
            "test",
            Some("visible"),
            None,
            None,
            25,
        )
        .unwrap();
        assert_eq!(preview.sessions.len(), 1);
        let body = list(
            &sources,
            &tmp.path().join("archives"),
            "test",
            Some("secret needle"),
            None,
            None,
            25,
        )
        .unwrap();
        assert!(body.sessions.is_empty());
    }

    #[test]
    fn transcript_messages_are_normalized_and_cursor_paged() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("sessions/2026/08/31");
        fs::create_dir_all(&root).unwrap();
        fs::write(
            root.join("rollout.jsonl"),
            concat!(
                "{\"timestamp\":\"2026-08-31T00:00:00Z\",\"type\":\"session_meta\",\"payload\":{\"id\":\"abc\",\"cwd\":\"/work/example\",\"source\":\"cli\",\"timestamp\":\"2026-08-31T00:00:00Z\"}}\n",
                "{\"timestamp\":\"2026-08-31T00:00:01Z\",\"type\":\"response_item\",\"payload\":{\"type\":\"message\",\"role\":\"user\",\"content\":[{\"type\":\"input_text\",\"text\":\"first\"}]}}\n",
                "{\"timestamp\":\"2026-08-31T00:00:02Z\",\"type\":\"response_item\",\"payload\":{\"type\":\"message\",\"role\":\"assistant\",\"content\":[{\"type\":\"output_text\",\"text\":\"second\"}]}}\n"
            ),
        )
        .unwrap();
        let sources = [HistorySource {
            provider: "codex".into(),
            agent_profile: None,
            root: tmp.path().join("sessions"),
        }];
        let history_id = stable_history_id("codex", None, "abc");
        let first = messages(&sources, &history_id, None, 1).unwrap();
        assert_eq!(first.messages[0].role, "user");
        assert_eq!(first.messages[0].text, "first");
        let second = messages(&sources, &history_id, first.next_cursor.as_deref(), 1).unwrap();
        assert_eq!(second.messages[0].role, "assistant");
        assert_eq!(second.messages[0].text, "second");
        assert_ne!(first.messages[0].id, second.messages[0].id);
    }

    #[test]
    fn catalog_reuses_a_snapshot_until_explicit_invalidation() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("sessions/2026/08/31");
        fs::create_dir_all(&root).unwrap();
        let write = |name: &str, id: &str| {
            fs::write(
                root.join(name),
                format!(
                    "{{\"timestamp\":\"2026-08-31T00:00:00Z\",\"type\":\"session_meta\",\"payload\":{{\"id\":\"{id}\",\"cwd\":\"/work/example\",\"source\":\"cli\",\"timestamp\":\"2026-08-31T00:00:00Z\"}}}}\n"
                ),
            )
            .unwrap();
        };
        write("one.jsonl", "one");
        let catalog = HistoryCatalog::new(
            vec![HistorySource {
                provider: "codex".into(),
                agent_profile: None,
                root: tmp.path().join("sessions"),
            }],
            tmp.path().join("archives"),
        );
        assert_eq!(
            catalog
                .list("test", None, None, None, 50)
                .unwrap()
                .sessions
                .len(),
            1
        );
        write("two.jsonl", "two");
        assert_eq!(
            catalog
                .list("test", None, None, None, 50)
                .unwrap()
                .sessions
                .len(),
            1
        );
        catalog.invalidate();
        assert_eq!(
            catalog
                .list("test", None, None, None, 50)
                .unwrap()
                .sessions
                .len(),
            2
        );
    }

    #[test]
    fn bounded_line_reader_discards_overflow_before_the_next_record() {
        let mut input = vec![b'x'; MAX_LINE_BYTES + 4096];
        input.extend_from_slice(b"\n{\"ok\":true}\n");
        let mut reader = BufReader::new(input.as_slice());
        let mut line = Vec::new();

        assert_eq!(
            read_bounded_line(&mut reader, &mut line).unwrap(),
            BoundedLine::Oversized
        );
        assert!(line.len() <= MAX_LINE_BYTES);
        assert_eq!(
            read_bounded_line(&mut reader, &mut line).unwrap(),
            BoundedLine::Line
        );
        assert_eq!(line, b"{\"ok\":true}\n");
    }

    #[test]
    fn list_cursor_is_stable_when_a_newer_session_is_inserted() {
        let original = CatalogSnapshot {
            sessions: vec![
                history_session("a", "2026-08-31T03:00:00Z"),
                history_session("b", "2026-08-31T02:00:00Z"),
                history_session("c", "2026-08-31T01:00:00Z"),
            ],
            truncated: false,
        };
        let first = paginate_snapshot(original, "test", None, None, None, 2).unwrap();
        assert_eq!(
            first
                .sessions
                .iter()
                .map(|session| session.id.as_str())
                .collect::<Vec<_>>(),
            vec!["a", "b"]
        );

        let refreshed = CatalogSnapshot {
            sessions: vec![
                history_session("new", "2026-08-31T04:00:00Z"),
                history_session("a", "2026-08-31T03:00:00Z"),
                history_session("b", "2026-08-31T02:00:00Z"),
                history_session("c", "2026-08-31T01:00:00Z"),
            ],
            truncated: false,
        };
        let second = paginate_snapshot(
            refreshed,
            "test",
            None,
            None,
            first.next_cursor.as_deref(),
            2,
        )
        .unwrap();
        assert_eq!(
            second
                .sessions
                .iter()
                .map(|session| session.id.as_str())
                .collect::<Vec<_>>(),
            vec!["c"]
        );
    }

    #[test]
    fn claude_history_excludes_sidechains_and_pages_visible_messages() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("projects/repo");
        fs::create_dir_all(&root).unwrap();
        fs::write(
            root.join("claude-id.jsonl"),
            concat!(
                "{\"sessionId\":\"claude-id\",\"cwd\":\"/work/claude\",\"type\":\"user\",\"message\":{\"content\":\"first\"}}\n",
                "{\"sessionId\":\"claude-id\",\"cwd\":\"/work/claude\",\"type\":\"assistant\",\"isSidechain\":true,\"message\":{\"content\":\"hidden\"}}\n",
                "{\"sessionId\":\"claude-id\",\"cwd\":\"/work/claude\",\"type\":\"assistant\",\"message\":{\"content\":[{\"type\":\"text\",\"text\":\"second\"}]}}\n"
            ),
        )
        .unwrap();
        let sources = [HistorySource {
            provider: "claude".into(),
            agent_profile: None,
            root: tmp.path().join("projects"),
        }];

        let page = list(
            &sources,
            &tmp.path().join("archives"),
            "test",
            Some("first"),
            Some("claude"),
            None,
            10,
        )
        .unwrap();
        assert_eq!(page.sessions.len(), 1);
        let first = messages(&sources, &page.sessions[0].id, None, 1).unwrap();
        assert_eq!(first.messages[0].text, "first");
        let second = messages(
            &sources,
            &page.sessions[0].id,
            first.next_cursor.as_deref(),
            10,
        )
        .unwrap();
        assert_eq!(second.messages.len(), 1);
        assert_eq!(second.messages[0].text, "second");
    }

    #[test]
    fn archives_do_not_cross_profile_identity_boundaries() {
        let tmp = tempfile::tempdir().unwrap();
        let profile_a_root = tmp.path().join("profile-a/2026/08/31");
        let profile_b_root = tmp.path().join("profile-b/2026/08/31");
        fs::create_dir_all(&profile_a_root).unwrap();
        fs::create_dir_all(&profile_b_root).unwrap();
        fs::write(
            profile_a_root.join("rollout.jsonl"),
            concat!(
                "{\"timestamp\":\"2026-08-31T00:00:00Z\",\"type\":\"session_meta\",\"payload\":{\"id\":\"shared-id\",\"cwd\":\"/work/profile-a\",\"source\":\"cli\",\"timestamp\":\"2026-08-31T00:00:00Z\"}}\n",
                "{\"timestamp\":\"2026-08-31T00:00:01Z\",\"type\":\"response_item\",\"payload\":{\"type\":\"message\",\"role\":\"user\",\"content\":[{\"type\":\"input_text\",\"text\":\"profile a transcript\"}]}}\n"
            ),
        )
        .unwrap();
        fs::write(
            profile_b_root.join("rollout.jsonl"),
            concat!(
                "{\"timestamp\":\"2026-08-31T00:00:00Z\",\"type\":\"session_meta\",\"payload\":{\"id\":\"shared-id\",\"cwd\":\"/work/profile-b\",\"source\":\"cli\",\"timestamp\":\"2026-08-31T00:00:00Z\"}}\n",
                "{\"timestamp\":\"2026-08-31T00:00:01Z\",\"type\":\"response_item\",\"payload\":{\"type\":\"message\",\"role\":\"user\",\"content\":[{\"type\":\"input_text\",\"text\":\"profile b transcript\"}]}}\n"
            ),
        )
        .unwrap();
        let sources = [
            HistorySource {
                provider: "codex".into(),
                agent_profile: Some("profile-a".into()),
                root: tmp.path().join("profile-a"),
            },
            HistorySource {
                provider: "codex".into(),
                agent_profile: Some("profile-b".into()),
                root: tmp.path().join("profile-b"),
            },
        ];
        let archives = tmp.path().join("archives");
        let profile_a_id = stable_history_id("codex", Some("profile-a"), "shared-id");
        let profile_b_id = stable_history_id("codex", Some("profile-b"), "shared-id");
        write_archive(
            &archives,
            &ArchivedSession {
                schema_version: "agent-session.history-archive.v1".into(),
                history_id: profile_a_id.clone(),
                provider: "codex".into(),
                provider_session_id: "shared-id".into(),
                agent_profile: Some("profile-a".into()),
                title: Some("Archived profile A".into()),
                cwd: "/work/profile-a".into(),
                created_at: "2026-08-31T00:00:00Z".into(),
                updated_at: "2026-08-31T00:00:01Z".into(),
                archived_at: "2026-08-31T00:00:02Z".into(),
            },
        )
        .unwrap()
        .commit();

        let page = list(&sources, &archives, "test", None, None, None, 10).unwrap();
        assert_eq!(page.sessions.len(), 2);
        let profile_a = page
            .sessions
            .iter()
            .find(|session| session.id == profile_a_id)
            .unwrap();
        assert_eq!(profile_a.title.as_deref(), Some("Archived profile A"));
        assert!(profile_a.archived_at.is_some());
        let profile_b = page
            .sessions
            .iter()
            .find(|session| session.id == profile_b_id)
            .unwrap();
        assert_eq!(profile_b.agent_profile.as_deref(), Some("profile-b"));
        assert_eq!(profile_b.cwd, "/work/profile-b");
        assert!(profile_b.archived_at.is_none());
        assert_eq!(
            messages(&sources, &profile_b_id, None, 10)
                .unwrap()
                .messages[0]
                .text,
            "profile b transcript"
        );
    }

    #[test]
    fn failed_rearchive_restores_previously_committed_metadata() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("archives");
        let mut archive = ArchivedSession {
            schema_version: "agent-session.history-archive.v1".to_string(),
            history_id: stable_history_id("codex", None, "abc"),
            provider: "codex".to_string(),
            provider_session_id: "abc".to_string(),
            agent_profile: None,
            title: Some("original".to_string()),
            cwd: "/work/example".to_string(),
            created_at: "2026-08-31T00:00:00Z".to_string(),
            updated_at: "2026-08-31T00:00:01Z".to_string(),
            archived_at: "2026-08-31T00:00:02Z".to_string(),
        };
        write_archive(&root, &archive).unwrap().commit();
        let path = root.join(format!("{}.json", archive.history_id));
        let original = fs::read(&path).unwrap();

        archive.title = Some("replacement".to_string());
        let pending = write_archive(&root, &archive).unwrap();
        drop(pending);

        assert_eq!(fs::read(path).unwrap(), original);
    }
}

use std::collections::BTreeSet;
use std::fs::{self, File};
use std::io::{self, Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use serde_json::Value;
use uuid::Uuid;

use crate::{
    AgentKind, SessionRecord, claude_projects_root, codex_sessions_root, read_claude_session_cwd,
    read_codex_session_meta,
};

pub(crate) const PROVIDER_PROMPT_CAPABILITY: &str = "provider-prompt.v1";
pub(crate) const MAX_PROVIDER_PROMPT_BYTES: usize = 16 * 1024;
const MAX_PROVIDER_LINE_BYTES: usize = 256 * 1024;
const MAX_PROVIDER_READ_BYTES: usize = 64 * 1024;
const MAX_PROVIDER_RESOLVE_ENTRIES: usize = 5_000;
const MAX_PROVIDER_RESOLVE_DEPTH: usize = 6;
const CLAUDE_FALLBACK_DELAY: Duration = Duration::from_millis(750);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ProviderKind {
    Codex,
    Claude,
}

impl ProviderKind {
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::Codex => "codex",
            Self::Claude => "claude",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ProviderPromptEvent {
    pub(crate) id: String,
    pub(crate) prompt: String,
    pub(crate) truncated: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct ParsedPrompt {
    prompt: String,
    truncated: bool,
}

#[derive(Debug, Clone)]
struct ProviderPromptSource {
    provider: ProviderKind,
    session_id: String,
    path: PathBuf,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct FileIdentity {
    device: u64,
    inode: u64,
}

#[derive(Debug)]
pub(crate) struct ProviderPromptTail {
    source: ProviderPromptSource,
    identity: FileIdentity,
    offset: u64,
    partial: Vec<u8>,
    discarding_oversized_line: bool,
    pending_claude: Option<ParsedPrompt>,
    pending_claude_since: Option<Instant>,
    claude_fallback_delay: Duration,
    disabled: bool,
}

impl ProviderPromptTail {
    pub(crate) fn open(record: &SessionRecord) -> Option<Self> {
        let source = resolve_provider_prompt_source(record)?;
        Self::open_source(source, CLAUDE_FALLBACK_DELAY).ok()
    }

    fn open_source(
        source: ProviderPromptSource,
        claude_fallback_delay: Duration,
    ) -> io::Result<Self> {
        let metadata = fs::metadata(&source.path)?;
        Ok(Self {
            source,
            identity: file_identity(&metadata),
            offset: metadata.len(),
            partial: Vec::new(),
            discarding_oversized_line: false,
            pending_claude: None,
            pending_claude_since: None,
            claude_fallback_delay,
            disabled: false,
        })
    }

    #[cfg(test)]
    fn open_path(
        provider: ProviderKind,
        session_id: &str,
        path: PathBuf,
        claude_fallback_delay: Duration,
    ) -> io::Result<Self> {
        Self::open_source(
            ProviderPromptSource {
                provider,
                session_id: session_id.to_string(),
                path,
            },
            claude_fallback_delay,
        )
    }

    pub(crate) fn provider(&self) -> ProviderKind {
        self.source.provider
    }

    pub(crate) fn poll(&mut self) -> io::Result<Vec<ProviderPromptEvent>> {
        if self.disabled {
            return Ok(Vec::new());
        }

        let metadata = fs::metadata(&self.source.path)?;
        let identity = file_identity(&metadata);
        if identity != self.identity || metadata.len() < self.offset {
            self.reset_to_eof(metadata, identity);
            if !source_still_matches(&self.source) {
                self.disabled = true;
            }
            return Ok(Vec::new());
        }

        let available = metadata.len().saturating_sub(self.offset);
        let read_len = usize::try_from(available)
            .unwrap_or(usize::MAX)
            .min(MAX_PROVIDER_READ_BYTES);
        let mut events = Vec::new();
        if read_len > 0 {
            let mut file = File::open(&self.source.path)?;
            file.seek(SeekFrom::Start(self.offset))?;
            let mut bytes = vec![0u8; read_len];
            let read = file.read(&mut bytes)?;
            self.offset = self.offset.saturating_add(read as u64);
            self.consume(&bytes[..read], &mut events);
        }

        if self.source.provider == ProviderKind::Claude
            && self
                .pending_claude_since
                .is_some_and(|started| started.elapsed() >= self.claude_fallback_delay)
            && let Some(prompt) = self.pending_claude.take()
        {
            self.pending_claude_since = None;
            events.push(provider_event(prompt));
        }
        Ok(events)
    }

    fn reset_to_eof(&mut self, metadata: fs::Metadata, identity: FileIdentity) {
        self.identity = identity;
        self.offset = metadata.len();
        self.partial.clear();
        self.discarding_oversized_line = false;
        self.pending_claude = None;
        self.pending_claude_since = None;
    }

    fn consume(&mut self, bytes: &[u8], events: &mut Vec<ProviderPromptEvent>) {
        for &byte in bytes {
            if byte == b'\n' {
                if !self.discarding_oversized_line {
                    if self.partial.last() == Some(&b'\r') {
                        self.partial.pop();
                    }
                    if let Ok(line) = std::str::from_utf8(&self.partial) {
                        self.consume_line(line.to_string(), events);
                    }
                }
                self.partial.clear();
                self.discarding_oversized_line = false;
                continue;
            }
            if self.discarding_oversized_line {
                continue;
            }
            if self.partial.len() >= MAX_PROVIDER_LINE_BYTES {
                self.partial.clear();
                self.discarding_oversized_line = true;
                continue;
            }
            self.partial.push(byte);
        }
    }

    fn consume_line(&mut self, line: String, events: &mut Vec<ProviderPromptEvent>) {
        match self.source.provider {
            ProviderKind::Codex => {
                if let Some(prompt) = parse_codex_prompt(&line) {
                    events.push(provider_event(prompt));
                }
            }
            ProviderKind::Claude => {
                if let Some(prompt) = parse_claude_last_prompt(&line, &self.source.session_id) {
                    if self.pending_claude.take().is_some() {
                        events.push(provider_event(prompt));
                    }
                    self.pending_claude_since = None;
                    return;
                }
                if let Some(prompt) = parse_claude_user_prompt(&line, &self.source.session_id) {
                    if let Some(previous) = self.pending_claude.replace(prompt) {
                        events.push(provider_event(previous));
                    }
                    self.pending_claude_since = Some(Instant::now());
                }
            }
        }
    }
}

fn provider_event(prompt: ParsedPrompt) -> ProviderPromptEvent {
    ProviderPromptEvent {
        id: format!("pp-{}", Uuid::new_v4().simple()),
        prompt: prompt.prompt,
        truncated: prompt.truncated,
    }
}

fn parse_codex_prompt(line: &str) -> Option<ParsedPrompt> {
    let value: Value = serde_json::from_str(line).ok()?;
    if value.get("type").and_then(Value::as_str) != Some("event_msg") {
        return None;
    }
    let payload = value.get("payload")?;
    if payload.get("type").and_then(Value::as_str) != Some("user_message") {
        return None;
    }
    bounded_prompt(payload.get("message").and_then(Value::as_str)?)
}

fn parse_claude_last_prompt(line: &str, session_id: &str) -> Option<ParsedPrompt> {
    let value: Value = serde_json::from_str(line).ok()?;
    if value.get("type").and_then(Value::as_str) != Some("last-prompt")
        || !claude_session_matches(&value, session_id)
    {
        return None;
    }
    bounded_prompt(value.get("lastPrompt").and_then(Value::as_str)?)
}

fn parse_claude_user_prompt(line: &str, session_id: &str) -> Option<ParsedPrompt> {
    let value: Value = serde_json::from_str(line).ok()?;
    if value.get("type").and_then(Value::as_str) != Some("user")
        || !claude_session_matches(&value, session_id)
        || value.get("isSidechain").and_then(Value::as_bool) == Some(true)
        || value.get("isMeta").and_then(Value::as_bool) == Some(true)
        || value.get("promptSource").and_then(Value::as_str) == Some("system")
    {
        return None;
    }
    let message = value.get("message")?;
    if message.get("role").and_then(Value::as_str) != Some("user") {
        return None;
    }
    let content = message.get("content")?;
    if let Some(text) = content.as_str() {
        return bounded_prompt(text);
    }
    let text = content
        .as_array()?
        .iter()
        .filter(|item| item.get("type").and_then(Value::as_str) == Some("text"))
        .filter_map(|item| item.get("text").and_then(Value::as_str))
        .filter(|text| !text.trim().is_empty())
        .collect::<Vec<_>>()
        .join("\n");
    bounded_prompt(&text)
}

fn claude_session_matches(value: &Value, session_id: &str) -> bool {
    value
        .get("sessionId")
        .or_else(|| value.get("session_id"))
        .and_then(Value::as_str)
        == Some(session_id)
}

fn bounded_prompt(prompt: &str) -> Option<ParsedPrompt> {
    if prompt.trim().is_empty() {
        return None;
    }
    if prompt.len() <= MAX_PROVIDER_PROMPT_BYTES {
        return Some(ParsedPrompt {
            prompt: prompt.to_string(),
            truncated: false,
        });
    }
    let mut end = MAX_PROVIDER_PROMPT_BYTES;
    while !prompt.is_char_boundary(end) {
        end = end.saturating_sub(1);
    }
    Some(ParsedPrompt {
        prompt: prompt[..end].to_string(),
        truncated: true,
    })
}

fn resolve_provider_prompt_source(record: &SessionRecord) -> Option<ProviderPromptSource> {
    resolve_provider_prompt_source_from_roots(
        record,
        codex_sessions_root().as_deref(),
        claude_projects_root().as_deref(),
    )
}

fn resolve_provider_prompt_source_from_roots(
    record: &SessionRecord,
    codex_root: Option<&Path>,
    claude_root: Option<&Path>,
) -> Option<ProviderPromptSource> {
    let agent = AgentKind::from_name(&record.agent)?;
    let resume = record.provider_resume.as_ref()?;
    if resume.provider != agent.as_str() || resume.session_id.trim().is_empty() {
        return None;
    }
    let (provider, path) = match agent {
        AgentKind::Codex => (
            ProviderKind::Codex,
            resolve_codex_transcript(codex_root?, &resume.session_id)?,
        ),
        AgentKind::Claude => (
            ProviderKind::Claude,
            resolve_claude_transcript(claude_root?, &resume.session_id)?,
        ),
        AgentKind::Hermes => return None,
    };
    Some(ProviderPromptSource {
        provider,
        session_id: resume.session_id.clone(),
        path,
    })
}

fn resolve_codex_transcript(root: &Path, session_id: &str) -> Option<PathBuf> {
    let mut paths = BTreeSet::new();
    let mut budget = ResolveBudget::new();
    collect_codex_transcripts(root, 0, session_id, &mut paths, &mut budget);
    if budget.truncated {
        return None;
    }
    single_path(paths)
}

fn collect_codex_transcripts(
    dir: &Path,
    depth: usize,
    session_id: &str,
    paths: &mut BTreeSet<PathBuf>,
    budget: &mut ResolveBudget,
) {
    if depth > MAX_PROVIDER_RESOLVE_DEPTH {
        return;
    }
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        if !budget.visit() {
            return;
        }
        let path = entry.path();
        let Ok(file_type) = entry.file_type() else {
            continue;
        };
        if file_type.is_dir() {
            collect_codex_transcripts(&path, depth + 1, session_id, paths, budget);
            if budget.truncated {
                return;
            }
            continue;
        }
        if path.extension().and_then(|value| value.to_str()) == Some("jsonl")
            && read_codex_session_meta(&path)
                .is_some_and(|metadata| metadata.session_id == session_id)
        {
            paths.insert(path);
        }
    }
}

fn resolve_claude_transcript(root: &Path, session_id: &str) -> Option<PathBuf> {
    let mut paths = BTreeSet::new();
    let mut budget = ResolveBudget::new();
    collect_claude_transcripts(root, 0, session_id, &mut paths, &mut budget);
    if budget.truncated {
        return None;
    }
    single_path(paths)
}

fn collect_claude_transcripts(
    dir: &Path,
    depth: usize,
    session_id: &str,
    paths: &mut BTreeSet<PathBuf>,
    budget: &mut ResolveBudget,
) {
    if depth > MAX_PROVIDER_RESOLVE_DEPTH {
        return;
    }
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        if !budget.visit() {
            return;
        }
        let path = entry.path();
        let Ok(file_type) = entry.file_type() else {
            continue;
        };
        if file_type.is_dir() {
            collect_claude_transcripts(&path, depth + 1, session_id, paths, budget);
            if budget.truncated {
                return;
            }
            continue;
        }
        if path.extension().and_then(|value| value.to_str()) == Some("jsonl")
            && read_claude_session_cwd(&path, session_id).is_some()
        {
            paths.insert(path);
        }
    }
}

#[derive(Debug)]
struct ResolveBudget {
    visited: usize,
    truncated: bool,
}

impl ResolveBudget {
    fn new() -> Self {
        Self {
            visited: 0,
            truncated: false,
        }
    }

    fn visit(&mut self) -> bool {
        if self.visited >= MAX_PROVIDER_RESOLVE_ENTRIES {
            self.truncated = true;
            return false;
        }
        self.visited += 1;
        true
    }
}

fn single_path(paths: BTreeSet<PathBuf>) -> Option<PathBuf> {
    (paths.len() == 1).then(|| paths.into_iter().next().expect("one transcript"))
}

fn source_still_matches(source: &ProviderPromptSource) -> bool {
    match source.provider {
        ProviderKind::Codex => read_codex_session_meta(&source.path)
            .is_some_and(|metadata| metadata.session_id == source.session_id),
        ProviderKind::Claude => read_claude_session_cwd(&source.path, &source.session_id).is_some(),
    }
}

#[cfg(unix)]
fn file_identity(metadata: &fs::Metadata) -> FileIdentity {
    use std::os::unix::fs::MetadataExt;
    FileIdentity {
        device: metadata.dev(),
        inode: metadata.ino(),
    }
}

#[cfg(not(unix))]
fn file_identity(metadata: &fs::Metadata) -> FileIdentity {
    let modified = metadata
        .modified()
        .ok()
        .and_then(|time| time.duration_since(std::time::SystemTime::UNIX_EPOCH).ok())
        .map_or(0, |duration| duration.as_nanos() as u64);
    FileIdentity {
        device: metadata.len(),
        inode: modified,
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;
    use std::fs::OpenOptions;
    use std::io::Write;

    use pretty_assertions::{assert_eq, assert_ne};
    use serde_json::json;

    use super::*;
    use crate::{ProviderResume, SESSION_DOCUMENT_VERSION};

    #[test]
    fn codex_parser_accepts_only_user_messages() {
        let submitted = r#"{"timestamp":"2099-01-01T00:00:00Z","type":"event_msg","payload":{"type":"user_message","message":"edited prompt","images":[],"local_images":[],"text_elements":[]}}"#;
        let agent_output = r#"{"timestamp":"2099-01-01T00:00:01Z","type":"event_msg","payload":{"type":"agent_message","message":"assistant output"}}"#;
        let injected_user = r#"{"timestamp":"2099-01-01T00:00:02Z","type":"response_item","payload":{"type":"message","role":"user","content":[{"type":"input_text","text":"system-injected context"}]}}"#;

        assert_eq!(
            parse_codex_prompt(submitted).map(|prompt| prompt.prompt),
            Some("edited prompt".to_string())
        );
        assert_eq!(parse_codex_prompt(agent_output), None);
        assert_eq!(parse_codex_prompt(injected_user), None);
    }

    #[test]
    fn claude_parser_prefers_last_prompt_and_filters_non_user_sources() {
        let last_prompt = r#"{"type":"last-prompt","sessionId":"claude-id","lastPrompt":"final edited prompt","leafUuid":"leaf"}"#;
        let typed = r#"{"type":"user","sessionId":"claude-id","isSidechain":false,"isMeta":false,"promptSource":"typed","message":{"role":"user","content":"typed prompt"}}"#;
        let sidechain = r#"{"type":"user","sessionId":"claude-id","isSidechain":true,"message":{"role":"user","content":"sidechain prompt"}}"#;
        let meta = r#"{"type":"user","sessionId":"claude-id","isMeta":true,"message":{"role":"user","content":"meta prompt"}}"#;
        let system = r#"{"type":"user","sessionId":"claude-id","promptSource":"system","message":{"role":"user","content":"system prompt"}}"#;
        let tool_result = r#"{"type":"user","sessionId":"claude-id","message":{"role":"user","content":[{"type":"tool_result","tool_use_id":"tool","content":"tool output"}]}}"#;

        assert_eq!(
            parse_claude_last_prompt(last_prompt, "claude-id").map(|prompt| prompt.prompt),
            Some("final edited prompt".to_string())
        );
        assert_eq!(
            parse_claude_user_prompt(typed, "claude-id").map(|prompt| prompt.prompt),
            Some("typed prompt".to_string())
        );
        assert_eq!(parse_claude_user_prompt(sidechain, "claude-id"), None);
        assert_eq!(parse_claude_user_prompt(meta, "claude-id"), None);
        assert_eq!(parse_claude_user_prompt(system, "claude-id"), None);
        assert_eq!(parse_claude_user_prompt(tool_result, "claude-id"), None);
        assert_eq!(parse_claude_last_prompt(last_prompt, "other-id"), None);
    }

    #[test]
    fn prompt_cap_truncates_at_utf8_boundary() {
        let prompt = format!("{}é", "a".repeat(MAX_PROVIDER_PROMPT_BYTES - 1));
        let parsed = bounded_prompt(&prompt).expect("prompt");
        assert!(parsed.truncated);
        assert_eq!(parsed.prompt.len(), MAX_PROVIDER_PROMPT_BYTES - 1);
        assert!(parsed.prompt.is_char_boundary(parsed.prompt.len()));
    }

    #[test]
    fn tail_starts_at_eof_and_handles_partial_and_repeated_prompts() {
        let tmp = tempfile::TempDir::new().expect("tempdir");
        let transcript = tmp.path().join("codex.jsonl");
        fs::write(&transcript, codex_line("old prompt")).expect("baseline");
        let mut tail = ProviderPromptTail::open_path(
            ProviderKind::Codex,
            "codex-id",
            transcript.clone(),
            Duration::ZERO,
        )
        .expect("tail");
        assert_eq!(tail.poll().expect("baseline poll"), Vec::new());

        let line = codex_line("same prompt");
        let split = line.len() / 2;
        append(&transcript, &line.as_bytes()[..split]);
        assert_eq!(tail.poll().expect("partial poll"), Vec::new());
        append(&transcript, &line.as_bytes()[split..]);
        append(&transcript, line.as_bytes());

        let events = tail.poll().expect("events");
        assert_eq!(events.len(), 2);
        assert_eq!(events[0].prompt, "same prompt");
        assert_eq!(events[1].prompt, "same prompt");
        assert_ne!(events[0].id, events[1].id);
    }

    #[test]
    fn tail_discards_oversized_lines_and_recovers_at_newline() {
        let tmp = tempfile::TempDir::new().expect("tempdir");
        let transcript = tmp.path().join("codex.jsonl");
        fs::write(&transcript, "").expect("baseline");
        let mut tail = ProviderPromptTail::open_path(
            ProviderKind::Codex,
            "codex-id",
            transcript.clone(),
            Duration::ZERO,
        )
        .expect("tail");
        append(
            &transcript,
            vec![b'x'; MAX_PROVIDER_LINE_BYTES + 1].as_slice(),
        );
        append(&transcript, b"\n");
        append(&transcript, codex_line("valid after oversize").as_bytes());

        let mut events = Vec::new();
        for _ in 0..8 {
            events.extend(tail.poll().expect("poll"));
        }
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].prompt, "valid after oversize");
    }

    #[test]
    fn tail_resets_to_eof_after_truncation_without_replay() {
        let tmp = tempfile::TempDir::new().expect("tempdir");
        let transcript = tmp.path().join("codex.jsonl");
        let meta = codex_meta("codex-id", "/repo");
        fs::write(&transcript, &meta).expect("baseline");
        let mut tail = ProviderPromptTail::open_path(
            ProviderKind::Codex,
            "codex-id",
            transcript.clone(),
            Duration::ZERO,
        )
        .expect("tail");
        append(&transcript, codex_line("before truncate").as_bytes());
        assert_eq!(tail.poll().expect("first").len(), 1);

        fs::write(&transcript, format!("{}{}", meta, codex_line("replayed"))).expect("truncate");
        assert_eq!(tail.poll().expect("reset"), Vec::new());
        append(&transcript, codex_line("after truncate").as_bytes());
        let events = tail.poll().expect("after reset");
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].prompt, "after truncate");
    }

    #[test]
    fn claude_tail_emits_last_prompt_once_and_falls_back_to_top_level_user() {
        let tmp = tempfile::TempDir::new().expect("tempdir");
        let transcript = tmp.path().join("claude.jsonl");
        fs::write(&transcript, "").expect("baseline");
        let mut tail = ProviderPromptTail::open_path(
            ProviderKind::Claude,
            "claude-id",
            transcript.clone(),
            Duration::ZERO,
        )
        .expect("tail");
        let user = claude_user("claude-id", "fallback prompt");
        let last = format!(
            "{}\n",
            json!({"type":"last-prompt","sessionId":"claude-id","lastPrompt":"canonical prompt"})
        );
        append(&transcript, format!("{user}{last}").as_bytes());
        let events = tail.poll().expect("preferred");
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].prompt, "canonical prompt");

        append(&transcript, last.as_bytes());
        assert_eq!(
            tail.poll().expect("repeated last-prompt snapshot"),
            Vec::new()
        );

        append(
            &transcript,
            claude_user("claude-id", "fallback only").as_bytes(),
        );
        let events = tail.poll().expect("fallback");
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].prompt, "fallback only");
    }

    #[test]
    fn resolver_requires_one_exact_provider_identity() {
        let tmp = tempfile::TempDir::new().expect("tempdir");
        let codex_root = tmp.path().join("codex");
        let claude_root = tmp.path().join("claude");
        let codex_file = codex_root.join("2026/07/session.jsonl");
        fs::create_dir_all(codex_file.parent().expect("parent")).expect("codex dirs");
        fs::write(&codex_file, codex_meta("codex-id", "/repo")).expect("codex transcript");
        let codex = record("codex", "codex-id");
        let source = resolve_provider_prompt_source_from_roots(
            &codex,
            Some(&codex_root),
            Some(&claude_root),
        )
        .expect("codex source");
        assert_eq!(source.provider, ProviderKind::Codex);
        assert_eq!(source.path, codex_file);

        let duplicate = codex_root.join("other.jsonl");
        fs::write(&duplicate, codex_meta("codex-id", "/repo")).expect("duplicate");
        assert!(
            resolve_provider_prompt_source_from_roots(
                &codex,
                Some(&codex_root),
                Some(&claude_root),
            )
            .is_none()
        );

        let unresolved = record("claude", "missing-id");
        assert!(
            resolve_provider_prompt_source_from_roots(
                &unresolved,
                Some(&codex_root),
                Some(&claude_root),
            )
            .is_none()
        );
    }

    fn append(path: &Path, bytes: &[u8]) {
        OpenOptions::new()
            .append(true)
            .open(path)
            .expect("open append")
            .write_all(bytes)
            .expect("append");
    }

    fn codex_line(prompt: &str) -> String {
        format!(
            "{}\n",
            json!({"type":"event_msg","payload":{"type":"user_message","message":prompt}})
        )
    }

    fn codex_meta(session_id: &str, cwd: &str) -> String {
        format!(
            "{}\n",
            json!({
                "timestamp":"2099-01-01T00:00:00Z",
                "type":"session_meta",
                "payload":{
                    "id":session_id,
                    "session_id":session_id,
                    "cwd":cwd,
                    "source":"cli",
                    "timestamp":"2099-01-01T00:00:00Z"
                }
            })
        )
    }

    fn claude_user(session_id: &str, prompt: &str) -> String {
        format!(
            "{}\n",
            json!({
                "type":"user",
                "sessionId":session_id,
                "cwd":"/repo",
                "message":{"role":"user","content":prompt}
            })
        )
    }

    fn record(agent: &str, session_id: &str) -> SessionRecord {
        SessionRecord {
            schema_version: SESSION_DOCUMENT_VERSION.to_string(),
            id: "session".to_string(),
            agent: agent.to_string(),
            mode: "interactive".to_string(),
            title: None,
            cwd: "/repo".to_string(),
            tmux_session: "hs-session".to_string(),
            prompt_file: None,
            log_file: None,
            created_at: "2099-01-01T00:00:00Z".to_string(),
            updated_at: "2099-01-01T00:00:00Z".to_string(),
            provider_resume: Some(ProviderResume {
                provider: agent.to_string(),
                session_id: session_id.to_string(),
                captured_at: "2099-01-01T00:00:00Z".to_string(),
                capture_method: "test".to_string(),
                resume_args: Vec::new(),
                extra: BTreeMap::new(),
            }),
            runtime: None,
            agent_args: Vec::new(),
            agent_bin: None,
            extra: BTreeMap::new(),
            resume_sidecar_extra: BTreeMap::new(),
        }
    }
}

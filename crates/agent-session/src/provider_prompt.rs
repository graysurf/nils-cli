use std::collections::VecDeque;
use std::fs::{self, File};
use std::io::{self, Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use serde_json::Value;
use uuid::Uuid;

use crate::{
    AgentKind, SessionRecord, claude_projects_root, codex_sessions_root, read_claude_session_cwd,
    read_codex_resumable_session_meta, resolve_provider_transcript_path_from_roots,
};

pub(crate) const PROVIDER_PROMPT_CAPABILITY: &str = "provider-prompt.v1";
pub(crate) const MAX_PROVIDER_PROMPT_BYTES: usize = 16 * 1024;
const MAX_PROVIDER_LINE_BYTES: usize = 256 * 1024;
const MAX_PROVIDER_READ_BYTES: usize = 64 * 1024;
const PROVIDER_CONTINUITY_BYTES: usize = 4 * 1024;
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
    pub(crate) submitted_at: String,
    pub(crate) truncated: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct ParsedPrompt {
    prompt: String,
    truncated: bool,
    submitted_at: Option<String>,
    turn_id: Option<String>,
}

#[derive(Debug)]
struct PendingClaudePrompt {
    prompt: ParsedPrompt,
    started_at: Instant,
    canonical: bool,
}

#[derive(Debug, Clone)]
pub(crate) struct ProviderPromptSource {
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
    continuity: Vec<u8>,
    partial: Vec<u8>,
    discarding_oversized_line: bool,
    pending_claude: VecDeque<PendingClaudePrompt>,
    claude_fallback_delay: Duration,
    disabled: bool,
}

impl ProviderPromptTail {
    pub(crate) fn resolve_source(record: &SessionRecord) -> Option<ProviderPromptSource> {
        resolve_provider_prompt_source(record)
    }

    pub(crate) fn open_source_at_eof(source: ProviderPromptSource) -> Option<Self> {
        Self::open_source(source, CLAUDE_FALLBACK_DELAY, true, true).ok()
    }

    pub(crate) fn open_new_runtime_source(source: ProviderPromptSource) -> Option<Self> {
        Self::open_source(source, CLAUDE_FALLBACK_DELAY, false, true).ok()
    }

    fn open_source(
        source: ProviderPromptSource,
        claude_fallback_delay: Duration,
        baseline_at_eof: bool,
        validate_source_identity: bool,
    ) -> io::Result<Self> {
        if validate_source_identity && !source_still_matches(&source) {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "provider transcript no longer matches the cached session identity",
            ));
        }
        let (mut file, metadata) = open_regular_file(&source.path)?;
        let identity = file_identity(&metadata);
        if validate_source_identity
            && (!source_still_matches(&source)
                || !opened_file_still_current(&source.path, identity))
        {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "provider transcript changed while validating cached session identity",
            ));
        }
        let offset = if baseline_at_eof { metadata.len() } else { 0 };
        Ok(Self {
            source,
            identity,
            offset,
            continuity: read_continuity(&mut file, offset)?,
            partial: Vec::new(),
            discarding_oversized_line: false,
            pending_claude: VecDeque::new(),
            claude_fallback_delay,
            disabled: false,
        })
    }

    #[cfg(test)]
    pub(crate) fn open_path(
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
            true,
            false,
        )
    }

    pub(crate) fn provider(&self) -> ProviderKind {
        self.source.provider
    }

    pub(crate) fn poll(&mut self) -> io::Result<Vec<ProviderPromptEvent>> {
        if self.disabled {
            return Ok(Vec::new());
        }

        let (mut file, metadata) = open_regular_file(&self.source.path)?;
        let identity = file_identity(&metadata);
        let continuous = identity == self.identity
            && metadata.len() >= self.offset
            && continuity_matches(&mut file, self.offset, &self.continuity)?;
        if !continuous {
            self.reset_to_eof(&mut file, &metadata, identity)?;
            if !source_still_matches(&self.source)
                || !opened_file_still_current(&self.source.path, identity)
            {
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
            file.seek(SeekFrom::Start(self.offset))?;
            let mut bytes = vec![0u8; read_len];
            let read = file.read(&mut bytes)?;
            self.offset = self.offset.saturating_add(read as u64);
            update_continuity(&mut self.continuity, &bytes[..read]);
            self.consume(&bytes[..read], &mut events);
        }

        if self.source.provider == ProviderKind::Claude {
            self.drain_ready_claude(&mut events);
        }
        Ok(events)
    }

    fn reset_to_eof(
        &mut self,
        file: &mut File,
        metadata: &fs::Metadata,
        identity: FileIdentity,
    ) -> io::Result<()> {
        self.identity = identity;
        self.offset = metadata.len();
        self.continuity = read_continuity(file, self.offset)?;
        self.partial.clear();
        self.discarding_oversized_line = false;
        self.pending_claude.clear();
        Ok(())
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
                    let match_index = match prompt.turn_id.as_deref() {
                        Some(turn_id) => self.pending_claude.iter().position(|candidate| {
                            candidate.prompt.turn_id.as_deref() == Some(turn_id)
                        }),
                        None if self.pending_claude.len() == 1
                            && self.pending_claude[0].prompt.turn_id.is_none() =>
                        {
                            Some(0)
                        }
                        None => None,
                    };
                    if let Some(index) = match_index {
                        let candidate = &mut self.pending_claude[index];
                        candidate.prompt.prompt = prompt.prompt;
                        candidate.prompt.truncated = prompt.truncated;
                        if prompt.submitted_at.is_some() {
                            candidate.prompt.submitted_at = prompt.submitted_at;
                        }
                        candidate.canonical = true;
                    }
                    return;
                }
                if let Some(prompt) = parse_claude_user_prompt(&line, &self.source.session_id) {
                    self.pending_claude.push_back(PendingClaudePrompt {
                        prompt,
                        started_at: Instant::now(),
                        canonical: false,
                    });
                }
            }
        }
    }

    fn drain_ready_claude(&mut self, events: &mut Vec<ProviderPromptEvent>) {
        while self.pending_claude.front().is_some_and(|candidate| {
            candidate.canonical || candidate.started_at.elapsed() >= self.claude_fallback_delay
        }) {
            let candidate = self
                .pending_claude
                .pop_front()
                .expect("ready candidate exists");
            events.push(provider_event(candidate.prompt));
        }
    }
}

fn provider_event(prompt: ParsedPrompt) -> ProviderPromptEvent {
    ProviderPromptEvent {
        id: format!("pp-{}", Uuid::new_v4().simple()),
        prompt: prompt.prompt,
        submitted_at: prompt
            .submitted_at
            .unwrap_or_else(|| jiff::Timestamp::now().to_string()),
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
    let mut prompt = bounded_prompt(payload.get("message").and_then(Value::as_str)?)?;
    prompt.submitted_at = provider_timestamp(&value);
    Some(prompt)
}

fn parse_claude_last_prompt(line: &str, session_id: &str) -> Option<ParsedPrompt> {
    let value: Value = serde_json::from_str(line).ok()?;
    if value.get("type").and_then(Value::as_str) != Some("last-prompt")
        || !claude_session_matches(&value, session_id)
    {
        return None;
    }
    let mut prompt = bounded_prompt(value.get("lastPrompt").and_then(Value::as_str)?)?;
    prompt.submitted_at = provider_timestamp(&value);
    prompt.turn_id = value
        .get("leafUuid")
        .or_else(|| value.get("leaf_uuid"))
        .and_then(Value::as_str)
        .map(str::to_string);
    Some(prompt)
}

fn parse_claude_user_prompt(line: &str, session_id: &str) -> Option<ParsedPrompt> {
    let value: Value = serde_json::from_str(line).ok()?;
    if value.get("type").and_then(Value::as_str) != Some("user")
        || !claude_session_matches(&value, session_id)
        || value.get("isSidechain").and_then(Value::as_bool) == Some(true)
        || value.get("isMeta").and_then(Value::as_bool) == Some(true)
        || value.get("isCompactSummary").and_then(Value::as_bool) == Some(true)
        || value
            .get("isVisibleInTranscriptOnly")
            .and_then(Value::as_bool)
            == Some(true)
        || value.get("promptSource").and_then(Value::as_str) == Some("system")
    {
        return None;
    }
    let message = value.get("message")?;
    if message.get("role").and_then(Value::as_str) != Some("user") {
        return None;
    }
    let content = message.get("content")?;
    let text = if let Some(text) = content.as_str() {
        text.to_string()
    } else {
        content
            .as_array()?
            .iter()
            .filter(|item| item.get("type").and_then(Value::as_str) == Some("text"))
            .filter_map(|item| item.get("text").and_then(Value::as_str))
            .filter(|text| !text.trim().is_empty())
            .collect::<Vec<_>>()
            .join("\n")
    };
    let mut prompt = bounded_prompt(&text)?;
    prompt.submitted_at = provider_timestamp(&value);
    prompt.turn_id = value
        .get("uuid")
        .or_else(|| value.get("messageUuid"))
        .and_then(Value::as_str)
        .map(str::to_string);
    Some(prompt)
}

fn provider_timestamp(value: &Value) -> Option<String> {
    value
        .get("timestamp")
        .or_else(|| value.get("submitted_at"))
        .and_then(Value::as_str)
        .filter(|timestamp| !timestamp.trim().is_empty())
        .and_then(|timestamp| timestamp.parse::<jiff::Timestamp>().ok())
        .map(|timestamp| timestamp.to_string())
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
            submitted_at: None,
            turn_id: None,
        });
    }
    let mut end = MAX_PROVIDER_PROMPT_BYTES;
    while !prompt.is_char_boundary(end) {
        end = end.saturating_sub(1);
    }
    Some(ParsedPrompt {
        prompt: prompt[..end].to_string(),
        truncated: true,
        submitted_at: None,
        turn_id: None,
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
            resolve_provider_transcript_path_from_roots(
                AgentKind::Codex,
                &resume.session_id,
                codex_root,
                claude_root,
            )?,
        ),
        AgentKind::Claude => (
            ProviderKind::Claude,
            resolve_provider_transcript_path_from_roots(
                AgentKind::Claude,
                &resume.session_id,
                codex_root,
                claude_root,
            )?,
        ),
        AgentKind::Hermes => return None,
    };
    Some(ProviderPromptSource {
        provider,
        session_id: resume.session_id.clone(),
        path,
    })
}

fn open_regular_file(path: &Path) -> io::Result<(File, fs::Metadata)> {
    let path_metadata = fs::symlink_metadata(path)?;
    if !path_metadata.file_type().is_file() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "provider transcript is not a regular file",
        ));
    }
    let file = File::open(path)?;
    let metadata = file.metadata()?;
    if file_identity(&path_metadata) != file_identity(&metadata) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "provider transcript changed while opening",
        ));
    }
    Ok((file, metadata))
}

fn opened_file_still_current(path: &Path, identity: FileIdentity) -> bool {
    fs::symlink_metadata(path)
        .ok()
        .filter(|metadata| metadata.file_type().is_file())
        .is_some_and(|metadata| file_identity(&metadata) == identity)
}

fn read_continuity(file: &mut File, offset: u64) -> io::Result<Vec<u8>> {
    let length = offset.min(PROVIDER_CONTINUITY_BYTES as u64) as usize;
    if length == 0 {
        return Ok(Vec::new());
    }
    file.seek(SeekFrom::Start(offset - length as u64))?;
    let mut bytes = vec![0; length];
    file.read_exact(&mut bytes)?;
    Ok(bytes)
}

fn continuity_matches(file: &mut File, offset: u64, expected: &[u8]) -> io::Result<bool> {
    if expected.is_empty() {
        return Ok(true);
    }
    file.seek(SeekFrom::Start(offset - expected.len() as u64))?;
    let mut actual = vec![0; expected.len()];
    file.read_exact(&mut actual)?;
    Ok(actual == expected)
}

fn update_continuity(continuity: &mut Vec<u8>, appended: &[u8]) {
    if appended.len() >= PROVIDER_CONTINUITY_BYTES {
        continuity.clear();
        continuity.extend_from_slice(&appended[appended.len() - PROVIDER_CONTINUITY_BYTES..]);
        return;
    }
    let overflow = continuity
        .len()
        .saturating_add(appended.len())
        .saturating_sub(PROVIDER_CONTINUITY_BYTES);
    if overflow > 0 {
        continuity.drain(..overflow);
    }
    continuity.extend_from_slice(appended);
}

fn source_still_matches(source: &ProviderPromptSource) -> bool {
    match source.provider {
        ProviderKind::Codex => read_codex_resumable_session_meta(&source.path)
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

    use nils_test_support::{EnvGuard, GlobalStateLock};
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
        let compact_summary = r#"{"type":"user","sessionId":"claude-id","isCompactSummary":true,"message":{"role":"user","content":"compact summary"}}"#;
        let transcript_only = r#"{"type":"user","sessionId":"claude-id","isVisibleInTranscriptOnly":true,"message":{"role":"user","content":"transcript-only context"}}"#;
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
        assert_eq!(parse_claude_user_prompt(compact_summary, "claude-id"), None);
        assert_eq!(parse_claude_user_prompt(transcript_only, "claude-id"), None);
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
    fn tail_rejects_same_inode_rewrite_that_regrows_past_the_offset() {
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
        append(&transcript, codex_line("before rewrite").as_bytes());
        assert_eq!(tail.poll().expect("first").len(), 1);

        let prefix_padding = tail.offset as usize - meta.len();
        let replacement = format!(
            "{}{}{}",
            meta,
            " ".repeat(prefix_padding),
            codex_line("replacement replay")
        );
        fs::write(&transcript, replacement).expect("same-inode rewrite");
        assert_eq!(tail.poll().expect("continuity reset"), Vec::new());
        append(&transcript, codex_line("after continuity reset").as_bytes());
        let events = tail.poll().expect("after continuity reset");
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].prompt, "after continuity reset");
    }

    #[test]
    fn tail_handles_matching_rotation_and_disables_mismatched_replacement() {
        let tmp = tempfile::TempDir::new().expect("tempdir");
        let transcript = tmp.path().join("codex.jsonl");
        let rotated = tmp.path().join("codex.old.jsonl");
        fs::write(&transcript, codex_meta("codex-id", "/repo")).expect("baseline");
        let mut tail = ProviderPromptTail::open_path(
            ProviderKind::Codex,
            "codex-id",
            transcript.clone(),
            Duration::ZERO,
        )
        .expect("tail");

        fs::rename(&transcript, &rotated).expect("rotate");
        fs::write(
            &transcript,
            format!(
                "{}{}",
                codex_meta("codex-id", "/repo"),
                codex_line("replayed after rotation")
            ),
        )
        .expect("matching replacement");
        assert_eq!(tail.poll().expect("matching reset"), Vec::new());
        append(&transcript, codex_line("after rotation").as_bytes());
        let events = tail.poll().expect("after matching rotation");
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].prompt, "after rotation");

        fs::remove_file(&rotated).expect("remove old rotation");
        fs::rename(&transcript, &rotated).expect("rotate again");
        fs::write(&transcript, codex_meta("other-id", "/repo")).expect("mismatch");
        assert_eq!(tail.poll().expect("mismatch reset"), Vec::new());
        append(&transcript, codex_line("must not emit").as_bytes());
        assert_eq!(tail.poll().expect("disabled"), Vec::new());
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
    fn claude_tail_waits_for_canonical_record_then_falls_back_after_deadline() {
        let tmp = tempfile::TempDir::new().expect("tempdir");
        let transcript = tmp.path().join("claude.jsonl");
        fs::write(&transcript, "").expect("baseline");
        let delay = Duration::from_millis(750);
        let mut tail = ProviderPromptTail::open_path(
            ProviderKind::Claude,
            "claude-id",
            transcript.clone(),
            delay,
        )
        .expect("tail");

        append(
            &transcript,
            claude_user_with_uuid("claude-id", "turn-a", "typed A").as_bytes(),
        );
        assert_eq!(tail.poll().expect("before deadline"), Vec::new());
        append(
            &transcript,
            claude_last_prompt("claude-id", "turn-a", "canonical A").as_bytes(),
        );
        let events = tail.poll().expect("canonical");
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].prompt, "canonical A");

        append(
            &transcript,
            claude_user_with_uuid("claude-id", "turn-b", "fallback B").as_bytes(),
        );
        assert_eq!(tail.poll().expect("fallback pending"), Vec::new());
        tail.pending_claude
            .front_mut()
            .expect("fallback candidate")
            .started_at = Instant::now() - delay;
        let events = tail.poll().expect("fallback expired");
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].prompt, "fallback B");
    }

    #[test]
    fn claude_tail_does_not_cross_pair_delayed_last_prompt_records() {
        let tmp = tempfile::TempDir::new().expect("tempdir");
        let transcript = tmp.path().join("claude.jsonl");
        fs::write(&transcript, "").expect("baseline");
        let delay = Duration::from_millis(750);
        let mut tail = ProviderPromptTail::open_path(
            ProviderKind::Claude,
            "claude-id",
            transcript.clone(),
            delay,
        )
        .expect("tail");
        append(
            &transcript,
            format!(
                "{}{}{}",
                claude_user_with_uuid("claude-id", "turn-a", "typed A"),
                claude_user_with_uuid("claude-id", "turn-b", "typed B"),
                claude_last_prompt("claude-id", "turn-a", "canonical A")
            )
            .as_bytes(),
        );
        let first = tail.poll().expect("canonical A");
        assert_eq!(
            first
                .iter()
                .map(|event| event.prompt.as_str())
                .collect::<Vec<_>>(),
            vec!["canonical A"]
        );
        tail.pending_claude
            .front_mut()
            .expect("fallback candidate")
            .started_at = Instant::now() - delay;
        let second = tail.poll().expect("fallback B");
        assert_eq!(
            second
                .iter()
                .map(|event| event.prompt.as_str())
                .collect::<Vec<_>>(),
            vec!["typed B"]
        );
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

    #[test]
    fn vscode_codex_prompt_source_opens_after_resolution() {
        let tmp = tempfile::TempDir::new().expect("tempdir");
        let codex_root = tmp.path().join("codex");
        let codex_file = codex_root.join("2026/07/session.jsonl");
        fs::create_dir_all(codex_file.parent().expect("parent")).expect("codex dirs");
        fs::write(
            &codex_file,
            format!(
                "{}\n",
                json!({
                    "timestamp":"2099-01-01T00:00:00Z",
                    "type":"session_meta",
                    "payload":{
                        "id":"codex-id",
                        "session_id":"codex-id",
                        "cwd":"/repo",
                        "source":"vscode",
                        "originator":"codex-tui",
                        "timestamp":"2099-01-01T00:00:00Z"
                    }
                })
            ),
        )
        .expect("codex transcript");
        let record = record("codex", "codex-id");
        let source = resolve_provider_prompt_source_from_roots(&record, Some(&codex_root), None)
            .expect("vscode prompt source");

        assert!(ProviderPromptTail::open_new_runtime_source(source).is_some());
    }

    #[cfg(unix)]
    #[test]
    fn resolver_ignores_symlinks_and_non_regular_transcripts() {
        use std::os::unix::fs::symlink;
        use std::os::unix::net::UnixListener;

        let tmp = tempfile::TempDir::new().expect("tempdir");
        let codex_root = tmp.path().join("codex");
        let outside = tmp.path().join("outside.jsonl");
        fs::create_dir_all(&codex_root).expect("root");
        fs::write(&outside, codex_meta("codex-id", "/repo")).expect("outside transcript");
        symlink(&outside, codex_root.join("linked.jsonl")).expect("symlink");
        let _socket = UnixListener::bind(codex_root.join("socket.jsonl")).expect("unix socket");

        let record = record("codex", "codex-id");
        assert!(
            resolve_provider_prompt_source_from_roots(&record, Some(&codex_root), None).is_none()
        );
    }

    #[test]
    fn resolver_fails_soft_when_shared_history_budget_is_exhausted() {
        let lock = GlobalStateLock::new();
        let tmp = tempfile::TempDir::new().expect("tempdir");
        let codex_root = tmp.path().join("codex");
        let transcript = codex_root.join("nested/session.jsonl");
        fs::create_dir_all(transcript.parent().expect("parent")).expect("dirs");
        fs::write(&transcript, codex_meta("codex-id", "/repo")).expect("transcript");
        let _entries = EnvGuard::set(&lock, "AGENT_SESSION_CODEX_RESUME_SCAN_MAX_ENTRIES", "1");

        let record = record("codex", "codex-id");
        assert!(
            resolve_provider_prompt_source_from_roots(&record, Some(&codex_root), None).is_none()
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

    fn claude_user_with_uuid(session_id: &str, uuid: &str, prompt: &str) -> String {
        format!(
            "{}\n",
            json!({
                "type":"user",
                "uuid":uuid,
                "sessionId":session_id,
                "cwd":"/repo",
                "timestamp":"2099-01-01T00:00:00Z",
                "message":{"role":"user","content":prompt}
            })
        )
    }

    fn claude_last_prompt(session_id: &str, leaf_uuid: &str, prompt: &str) -> String {
        format!(
            "{}\n",
            json!({
                "type":"last-prompt",
                "sessionId":session_id,
                "leafUuid":leaf_uuid,
                "lastPrompt":prompt
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
            title_revision: 0,
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

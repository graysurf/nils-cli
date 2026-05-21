//! Product version probes for `agent-runtime doctor`.

use super::{DoctorFinding, DoctorSeverity};
use crate::render::manifest::ProductRoot;
use std::cmp::Ordering;
use std::process::{Command, Output, Stdio};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

const PROBE_TIMEOUT_POLLS: usize = 50;
const PROBE_TIMEOUT_SLEEP: Duration = Duration::from_millis(100);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VersionStatus {
    Ok,
    RecommendedOnly,
    Warn,
    Outdated,
    Unparseable,
}

impl VersionStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            VersionStatus::Ok => "ok",
            VersionStatus::RecommendedOnly => "recommended-only",
            VersionStatus::Warn => "warn",
            VersionStatus::Outdated => "outdated",
            VersionStatus::Unparseable => "unparseable",
        }
    }
}

#[derive(Debug, Clone)]
pub struct VersionProbeInput {
    pub product: String,
    pub command: String,
    pub min_version: String,
    pub recommended_version: String,
    pub min_version_effective_from: String,
    pub raw_output: String,
    pub today: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VersionProbeFinding {
    pub product: String,
    pub command: String,
    pub status: VersionStatus,
    pub severity: DoctorSeverity,
    pub parsed_version: Option<String>,
    pub raw_output: String,
    pub message: String,
}

impl VersionProbeFinding {
    pub fn to_doctor_finding(&self) -> DoctorFinding {
        let message = if self.status == VersionStatus::Unparseable {
            format!(
                "status={} command=`{}` raw_output={:?}",
                self.status.as_str(),
                self.command,
                self.raw_output
            )
        } else {
            format!(
                "status={} parsed={} command=`{}`: {}",
                self.status.as_str(),
                self.parsed_version.as_deref().unwrap_or("unknown"),
                self.command,
                self.message
            )
        };

        match self.severity {
            DoctorSeverity::Ok => DoctorFinding {
                product: self.product.clone(),
                check: "version-probe",
                severity: DoctorSeverity::Ok,
                entry_id: None,
                path: None,
                message,
            },
            DoctorSeverity::Warn => {
                DoctorFinding::warn(&self.product, "version-probe", None, None, message)
            }
            DoctorSeverity::Block => {
                DoctorFinding::block(&self.product, "version-probe", None, None, message)
            }
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Version {
    major: u64,
    minor: u64,
    patch: u64,
}

impl Version {
    fn parse(raw: &str) -> Option<Self> {
        let bytes = raw.as_bytes();
        for i in 0..bytes.len() {
            if !(bytes[i].is_ascii_digit()
                || (bytes[i] == b'v' && bytes.get(i + 1).is_some_and(u8::is_ascii_digit)))
            {
                continue;
            }
            let start = if bytes[i] == b'v' { i + 1 } else { i };
            if start > 0 {
                let prev = bytes[start - 1];
                if prev.is_ascii_alphanumeric() && prev != b'v' {
                    continue;
                }
            }
            if let Some((version, _end)) = parse_at(bytes, start) {
                return Some(version);
            }
        }
        None
    }
}

impl Ord for Version {
    fn cmp(&self, other: &Self) -> Ordering {
        (self.major, self.minor, self.patch).cmp(&(other.major, other.minor, other.patch))
    }
}

impl PartialOrd for Version {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl std::fmt::Display for Version {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}.{}.{}", self.major, self.minor, self.patch)
    }
}

pub fn probe_product(product: &str, root: &ProductRoot) -> VersionProbeFinding {
    let raw_output = run_probe_command(&root.version_probe);
    classify(VersionProbeInput {
        product: product.to_string(),
        command: root.version_probe.clone(),
        min_version: root.min_version.clone(),
        recommended_version: root.recommended_version.clone(),
        min_version_effective_from: root.min_version_effective_from.clone(),
        raw_output,
        today: today_utc(),
    })
}

pub fn classify(input: VersionProbeInput) -> VersionProbeFinding {
    let Some(parsed) = Version::parse(&input.raw_output) else {
        return finding(
            input,
            VersionStatus::Unparseable,
            DoctorSeverity::Warn,
            None,
            "version output could not be parsed",
        );
    };

    let Some(minimum) = Version::parse(&input.min_version) else {
        return finding(
            input,
            VersionStatus::Unparseable,
            DoctorSeverity::Warn,
            Some(parsed),
            "min_version could not be parsed",
        );
    };
    let Some(recommended) = Version::parse(&input.recommended_version) else {
        return finding(
            input,
            VersionStatus::Unparseable,
            DoctorSeverity::Warn,
            Some(parsed),
            "recommended_version could not be parsed",
        );
    };

    if parsed >= recommended {
        return finding(
            input,
            VersionStatus::Ok,
            DoctorSeverity::Ok,
            Some(parsed),
            "version meets the recommended floor",
        );
    }

    if parsed >= minimum {
        return finding(
            input,
            VersionStatus::RecommendedOnly,
            DoctorSeverity::Warn,
            Some(parsed),
            "version meets the minimum floor but is below the recommended floor",
        );
    }

    if effective_date_has_passed(&input.today, &input.min_version_effective_from) {
        finding(
            input,
            VersionStatus::Outdated,
            DoctorSeverity::Block,
            Some(parsed),
            "version is below the minimum floor after the effective date",
        )
    } else {
        finding(
            input,
            VersionStatus::Warn,
            DoctorSeverity::Warn,
            Some(parsed),
            "version is below the minimum floor before the effective date",
        )
    }
}

fn finding(
    input: VersionProbeInput,
    status: VersionStatus,
    severity: DoctorSeverity,
    parsed_version: Option<Version>,
    message: &str,
) -> VersionProbeFinding {
    VersionProbeFinding {
        product: input.product,
        command: input.command,
        status,
        severity,
        parsed_version: parsed_version.map(|v| v.to_string()),
        raw_output: input.raw_output,
        message: message.to_string(),
    }
}

fn run_probe_command(command: &str) -> String {
    run_probe_command_with_timeout(command, PROBE_TIMEOUT_POLLS, PROBE_TIMEOUT_SLEEP)
}

fn run_probe_command_with_timeout(command: &str, polls: usize, sleep: Duration) -> String {
    let mut parts = command.split_whitespace();
    let Some(program) = parts.next() else {
        return String::new();
    };
    let mut child = match Command::new(program)
        .args(parts)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
    {
        Ok(child) => child,
        Err(source) => return format!("failed to run `{command}`: {source}"),
    };

    for _ in 0..polls {
        match child.try_wait() {
            Ok(Some(_)) => {
                return child
                    .wait_with_output()
                    .map(output_to_raw)
                    .unwrap_or_else(|source| {
                        format!("failed to read `{command}` output: {source}")
                    });
            }
            Ok(None) => std::thread::sleep(sleep),
            Err(source) => {
                let _ = child.kill();
                return format!("failed to wait for `{command}`: {source}");
            }
        }
    }

    let timeout_ms = polls as u128 * sleep.as_millis();
    let _ = child.kill();
    child
        .wait_with_output()
        .map(|output| {
            let mut raw = output_to_raw(output);
            if !raw.is_empty() {
                raw.push('\n');
            }
            raw.push_str(&format!("timed_out_after_ms={timeout_ms}"));
            raw
        })
        .unwrap_or_else(|source| format!("timed out running `{command}`; kill failed: {source}"))
}

fn output_to_raw(output: Output) -> String {
    let mut raw = String::new();
    raw.push_str(&String::from_utf8_lossy(&output.stdout));
    raw.push_str(&String::from_utf8_lossy(&output.stderr));
    if !output.status.success() {
        raw.push_str(&format!("\nexit_status={}", output.status));
    }
    raw.trim().to_string()
}

fn parse_at(bytes: &[u8], mut i: usize) -> Option<(Version, usize)> {
    let (major, next) = parse_number(bytes, i)?;
    i = next;
    if bytes.get(i) != Some(&b'.') {
        return None;
    }
    let (minor, next) = parse_number(bytes, i + 1)?;
    i = next;
    if bytes.get(i) != Some(&b'.') {
        return None;
    }
    let (patch, next) = parse_number(bytes, i + 1)?;
    if bytes
        .get(next)
        .is_some_and(|b| b.is_ascii_digit() || b.is_ascii_alphabetic() || *b == b'_')
    {
        return None;
    }
    Some((
        Version {
            major,
            minor,
            patch,
        },
        next,
    ))
}

fn parse_number(bytes: &[u8], mut i: usize) -> Option<(u64, usize)> {
    let start = i;
    let mut value = 0u64;
    while let Some(byte) = bytes.get(i) {
        if !byte.is_ascii_digit() {
            break;
        }
        value = value
            .saturating_mul(10)
            .saturating_add(u64::from(byte - b'0'));
        i += 1;
    }
    (i > start).then_some((value, i))
}

fn effective_date_has_passed(today: &str, effective_from: &str) -> bool {
    valid_date(today) && valid_date(effective_from) && today >= effective_from
}

fn valid_date(value: &str) -> bool {
    let bytes = value.as_bytes();
    bytes.len() == 10
        && bytes[0..4].iter().all(u8::is_ascii_digit)
        && bytes[4] == b'-'
        && bytes[5..7].iter().all(u8::is_ascii_digit)
        && bytes[7] == b'-'
        && bytes[8..10].iter().all(u8::is_ascii_digit)
}

fn today_utc() -> String {
    if let Ok(today) = std::env::var("AGENT_RUNTIME_DOCTOR_TODAY")
        && valid_date(&today)
    {
        return today;
    }

    // Doctor is a read-only host posture command, not part of the render
    // determinism path. The date gates a published version-floor deadline.
    #[allow(clippy::disallowed_methods)]
    let now = SystemTime::now();
    let days = now.duration_since(UNIX_EPOCH).unwrap_or_default().as_secs() / 86_400;
    let (year, month, day) = civil_from_days(days as i64);
    format!("{year:04}-{month:02}-{day:02}")
}

fn civil_from_days(days_since_unix_epoch: i64) -> (i64, i64, i64) {
    let z = days_since_unix_epoch + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1_460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = mp + if mp < 10 { 3 } else { -9 };
    let year = y + if month <= 2 { 1 } else { 0 };
    (year, month, day)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::os::unix::fs::PermissionsExt;
    use tempfile::TempDir;

    #[test]
    fn semver_matcher_tolerates_common_prefixes() {
        assert_eq!(
            Version::parse("codex 0.18.2 (build abc1234)").map(|v| v.to_string()),
            Some("0.18.2".to_string())
        );
        assert_eq!(
            Version::parse("claude-code v2.1.145").map(|v| v.to_string()),
            Some("2.1.145".to_string())
        );
    }

    #[test]
    fn unix_epoch_date_conversion_is_stable() {
        assert_eq!(civil_from_days(0), (1970, 1, 1));
        assert_eq!(civil_from_days(20_229), (2025, 5, 21));
    }

    #[test]
    fn version_probe_timeout_is_loud_and_bounded() {
        let tmp = TempDir::new().unwrap();
        let script = tmp.path().join("slow-version");
        fs::write(&script, "#!/usr/bin/env sh\nsleep 10\n").unwrap();
        let mut perms = fs::metadata(&script).unwrap().permissions();
        perms.set_mode(0o755);
        fs::set_permissions(&script, perms).unwrap();

        let raw =
            run_probe_command_with_timeout(&script.to_string_lossy(), 0, Duration::from_millis(0));

        assert!(
            raw.contains("timed_out_after_ms=0"),
            "timeout should be explicit: {raw}"
        );
    }
}

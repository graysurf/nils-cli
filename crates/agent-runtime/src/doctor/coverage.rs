//! Required CLI and third-party tool coverage for `agent-runtime doctor`.

use super::{DoctorFinding, DoctorSeverity};
use crate::doctor::version::{self, Version};
use crate::render::manifest::{
    CliToolsManifest, SCHEMA_VERSION, SKILLS_SCHEMA_VERSIONS, SkillsManifest,
};
use std::collections::BTreeMap;
use std::ffi::OsString;
use std::path::{Path, PathBuf};
use thiserror::Error;

#[derive(Debug, Error)]
pub enum CoverageError {
    #[error("missing manifest: {path}")]
    Missing { path: PathBuf },
    #[error("schema_version mismatch in {file}: expected one of {expected:?}, got {found}")]
    SchemaVersion {
        file: PathBuf,
        expected: Vec<u32>,
        found: u32,
    },
    #[error("parse error in {file}: {source}")]
    Parse {
        file: PathBuf,
        #[source]
        source: serde_yaml_ng::Error,
    },
    #[error("invalid skills manifest contract in {file}: {message}")]
    InvalidSkills { file: PathBuf, message: String },
    #[error("io error reading {file}: {source}")]
    Io {
        file: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("unknown cli-tools profile `{profile}`; expected core, recommended, or full")]
    UnknownProfile { profile: String },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CoverageKind {
    RequiredCli,
    CliTool,
}

impl CoverageKind {
    pub fn check(self) -> &'static str {
        match self {
            CoverageKind::RequiredCli => "required-cli",
            CoverageKind::CliTool => "cli-tool",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CoverageStatus {
    Ok,
    Missing,
    Outdated,
    Unparseable,
}

impl CoverageStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            CoverageStatus::Ok => "ok",
            CoverageStatus::Missing => "missing",
            CoverageStatus::Outdated => "outdated",
            CoverageStatus::Unparseable => "unparseable",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CoverageFinding {
    pub kind: CoverageKind,
    pub name: String,
    pub command: String,
    pub status: CoverageStatus,
    pub severity: DoctorSeverity,
    pub required_version: Option<String>,
    pub parsed_version: Option<String>,
    pub formula: Option<String>,
    pub message: String,
}

impl CoverageFinding {
    pub fn to_doctor_finding(&self, product: &str) -> DoctorFinding {
        let mut message = format!("status={} command=`{}`", self.status.as_str(), self.command);
        if let Some(required) = self.required_version.as_deref() {
            message.push_str(&format!(" required={required}"));
        }
        if let Some(parsed) = self.parsed_version.as_deref() {
            message.push_str(&format!(" parsed={parsed}"));
        }
        if let Some(formula) = self.formula.as_deref() {
            message.push_str(&format!(" upgrade=`brew upgrade {formula}`"));
        }
        if !self.message.is_empty() {
            message.push_str(&format!(": {}", self.message));
        }

        match self.severity {
            DoctorSeverity::Ok => DoctorFinding {
                product: product.to_string(),
                check: self.kind.check(),
                severity: DoctorSeverity::Ok,
                entry_id: Some(self.name.clone()),
                path: None,
                message,
            },
            DoctorSeverity::Warn => DoctorFinding::warn(
                product,
                self.kind.check(),
                Some(self.name.clone()),
                None,
                message,
            ),
            DoctorSeverity::Block => DoctorFinding::block(
                product,
                self.kind.check(),
                Some(self.name.clone()),
                None,
                message,
            ),
        }
    }
}

pub fn probe(source_root: &Path, profile: &str) -> Result<Vec<CoverageFinding>, CoverageError> {
    let skills_file = source_root.join("manifests").join("skills.yaml");
    let skills: SkillsManifest = load_manifest(&skills_file)?;
    skills
        .validate_for_file(&skills_file)
        .map_err(|err| CoverageError::InvalidSkills {
            file: skills_file,
            message: err.to_string(),
        })?;
    let cli_tools: CliToolsManifest =
        load_manifest(&source_root.join("manifests").join("cli-tools.yaml"))?;

    let mut findings = Vec::new();
    for (binary, floor) in required_cli_floors(&skills) {
        findings.push(probe_required_cli(&binary, &floor));
    }
    for key in profile_tools(&cli_tools, profile)? {
        let Some(formula) = cli_tools.formulas.get(key) else {
            findings.push(CoverageFinding {
                kind: CoverageKind::CliTool,
                name: key.to_string(),
                command: key.to_string(),
                status: CoverageStatus::Missing,
                severity: DoctorSeverity::Warn,
                required_version: None,
                parsed_version: None,
                formula: None,
                message: "profile references a formula key that is not declared".to_string(),
            });
            continue;
        };
        findings.push(probe_cli_tool(
            key,
            formula.command.as_str(),
            formula.brew.as_str(),
        ));
    }
    Ok(findings)
}

fn load_manifest<T>(file: &Path) -> Result<T, CoverageError>
where
    T: for<'de> serde::Deserialize<'de> + SchemaVersion,
{
    if !file.exists() {
        return Err(CoverageError::Missing {
            path: file.to_path_buf(),
        });
    }
    let raw = std::fs::read_to_string(file).map_err(|source| CoverageError::Io {
        file: file.to_path_buf(),
        source,
    })?;
    let parsed: T = serde_yaml_ng::from_str(&raw).map_err(|source| CoverageError::Parse {
        file: file.to_path_buf(),
        source,
    })?;
    let expected = T::supported_schema_versions();
    if !expected.contains(&parsed.schema_version()) {
        return Err(CoverageError::SchemaVersion {
            file: file.to_path_buf(),
            expected: expected.to_vec(),
            found: parsed.schema_version(),
        });
    }
    Ok(parsed)
}

trait SchemaVersion {
    fn schema_version(&self) -> u32;

    fn supported_schema_versions() -> &'static [u32]
    where
        Self: Sized,
    {
        &[SCHEMA_VERSION]
    }
}

impl SchemaVersion for SkillsManifest {
    fn schema_version(&self) -> u32 {
        self.schema_version
    }

    fn supported_schema_versions() -> &'static [u32] {
        SKILLS_SCHEMA_VERSIONS
    }
}

impl SchemaVersion for CliToolsManifest {
    fn schema_version(&self) -> u32 {
        self.schema_version
    }
}

fn required_cli_floors(skills: &SkillsManifest) -> BTreeMap<String, String> {
    let mut floors: BTreeMap<String, String> = BTreeMap::new();
    for skill in &skills.skills {
        for (binary, floor) in &skill.required_clis {
            match floors.get(binary.as_str()) {
                Some(existing) if floor_cmp(floor, existing).is_le() => {}
                _ => {
                    floors.insert(binary.clone(), floor.clone());
                }
            }
        }
    }
    floors
}

fn floor_cmp(left: &str, right: &str) -> std::cmp::Ordering {
    match (Version::parse(left), Version::parse(right)) {
        (Some(left), Some(right)) => left.cmp(&right),
        _ => left.cmp(right),
    }
}

fn profile_tools<'a>(
    cli_tools: &'a CliToolsManifest,
    profile: &str,
) -> Result<&'a [String], CoverageError> {
    match profile {
        "core" => Ok(&cli_tools.profiles.core),
        "recommended" => Ok(&cli_tools.profiles.recommended),
        "full" => Ok(&cli_tools.profiles.full),
        _ => Err(CoverageError::UnknownProfile {
            profile: profile.to_string(),
        }),
    }
}

fn probe_required_cli(binary: &str, floor: &str) -> CoverageFinding {
    if find_in_path(binary).is_none() {
        return CoverageFinding {
            kind: CoverageKind::RequiredCli,
            name: binary.to_string(),
            command: binary.to_string(),
            status: CoverageStatus::Missing,
            severity: DoctorSeverity::Block,
            required_version: Some(floor.to_string()),
            parsed_version: None,
            formula: Some("nils-cli".to_string()),
            message: "required nils-cli binary is not on PATH".to_string(),
        };
    }

    let raw = version::run_probe_command(&format!("{binary} --version"));
    let Some(parsed) = Version::parse(&raw) else {
        return CoverageFinding {
            kind: CoverageKind::RequiredCli,
            name: binary.to_string(),
            command: binary.to_string(),
            status: CoverageStatus::Unparseable,
            severity: DoctorSeverity::Warn,
            required_version: Some(floor.to_string()),
            parsed_version: None,
            formula: Some("nils-cli".to_string()),
            message: format!("version output could not be parsed: {raw:?}"),
        };
    };
    let Some(required) = Version::parse(floor) else {
        return CoverageFinding {
            kind: CoverageKind::RequiredCli,
            name: binary.to_string(),
            command: binary.to_string(),
            status: CoverageStatus::Unparseable,
            severity: DoctorSeverity::Warn,
            required_version: Some(floor.to_string()),
            parsed_version: Some(parsed.to_string()),
            formula: Some("nils-cli".to_string()),
            message: "required_clis floor could not be parsed".to_string(),
        };
    };

    if parsed < required {
        CoverageFinding {
            kind: CoverageKind::RequiredCli,
            name: binary.to_string(),
            command: binary.to_string(),
            status: CoverageStatus::Outdated,
            severity: DoctorSeverity::Warn,
            required_version: Some(floor.to_string()),
            parsed_version: Some(parsed.to_string()),
            formula: Some("nils-cli".to_string()),
            message: "installed nils-cli binary is below the declared floor".to_string(),
        }
    } else {
        CoverageFinding {
            kind: CoverageKind::RequiredCli,
            name: binary.to_string(),
            command: binary.to_string(),
            status: CoverageStatus::Ok,
            severity: DoctorSeverity::Ok,
            required_version: Some(floor.to_string()),
            parsed_version: Some(parsed.to_string()),
            formula: Some("nils-cli".to_string()),
            message: "installed nils-cli binary meets the declared floor".to_string(),
        }
    }
}

fn probe_cli_tool(key: &str, command: &str, brew_formula: &str) -> CoverageFinding {
    if find_in_path(command).is_none() {
        return CoverageFinding {
            kind: CoverageKind::CliTool,
            name: key.to_string(),
            command: command.to_string(),
            status: CoverageStatus::Missing,
            severity: DoctorSeverity::Warn,
            required_version: None,
            parsed_version: None,
            formula: Some(brew_formula.to_string()),
            message: format!(
                "cli-tools binary is not on PATH; install with `brew install {brew_formula}`"
            ),
        };
    }

    if brew_reports_outdated(brew_formula) {
        CoverageFinding {
            kind: CoverageKind::CliTool,
            name: key.to_string(),
            command: command.to_string(),
            status: CoverageStatus::Outdated,
            severity: DoctorSeverity::Warn,
            required_version: None,
            parsed_version: None,
            formula: Some(brew_formula.to_string()),
            message: "Homebrew reports a newer formula is available".to_string(),
        }
    } else {
        CoverageFinding {
            kind: CoverageKind::CliTool,
            name: key.to_string(),
            command: command.to_string(),
            status: CoverageStatus::Ok,
            severity: DoctorSeverity::Ok,
            required_version: None,
            parsed_version: None,
            formula: Some(brew_formula.to_string()),
            message: "cli-tools binary is on PATH".to_string(),
        }
    }
}

fn brew_reports_outdated(formula: &str) -> bool {
    if find_in_path("brew").is_none() {
        return false;
    }
    let raw = version::run_probe_command(&format!("brew outdated --quiet {formula}"));
    raw.lines().any(|line| line.trim() == formula)
}

fn find_in_path(program: &str) -> Option<PathBuf> {
    if program.contains('/') || program.contains('\\') {
        let path = PathBuf::from(program);
        return is_executable_file(&path).then_some(path);
    }
    let path_var = std::env::var_os("PATH")?;
    let windows_extensions = if cfg!(windows) {
        Some(windows_pathext_extensions())
    } else {
        None
    };
    for dir in std::env::split_paths(&path_var) {
        let candidate = dir.join(program);
        if is_executable_file(&candidate) {
            return Some(candidate);
        }
        if let Some(extensions) = windows_extensions.as_ref() {
            for extension in extensions {
                let mut file_name = OsString::from(program);
                file_name.push(extension);
                let candidate = dir.join(file_name);
                if is_executable_file(&candidate) {
                    return Some(candidate);
                }
            }
        }
    }
    None
}

fn windows_pathext_extensions() -> Vec<OsString> {
    let raw = std::env::var_os("PATHEXT")
        .unwrap_or_else(|| OsString::from(".COM;.EXE;.BAT;.CMD;.VBS;.VBE;.JS;.JSE;.WSF;.WSH"));
    raw.to_string_lossy()
        .split(';')
        .filter_map(|segment| {
            let segment = segment.trim();
            if segment.is_empty() {
                None
            } else if segment.starts_with('.') {
                Some(OsString::from(segment))
            } else {
                Some(OsString::from(format!(".{segment}")))
            }
        })
        .collect()
}

fn is_executable_file(path: &Path) -> bool {
    let Ok(meta) = std::fs::metadata(path) else {
        return false;
    };
    if !meta.is_file() {
        return false;
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        meta.permissions().mode() & 0o111 != 0
    }
    #[cfg(not(unix))]
    {
        true
    }
}

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
    #[error("schema_version mismatch in {file}: expected {expected}, got {found}")]
    SchemaVersion {
        file: PathBuf,
        expected: u32,
        found: u32,
    },
    #[error("schema_version mismatch in {file}: expected one of {expected:?}, got {found}")]
    SchemaVersions {
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
        return Err(if let [expected] = expected {
            CoverageError::SchemaVersion {
                file: file.to_path_buf(),
                expected: *expected,
                found: parsed.schema_version(),
            }
        } else {
            CoverageError::SchemaVersions {
                file: file.to_path_buf(),
                expected: expected.to_vec(),
                found: parsed.schema_version(),
            }
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

#[cfg(test)]
mod tests {
    use super::*;

    use pretty_assertions::assert_eq;
    use std::fs;
    use tempfile::TempDir;

    const CLI_TOOLS: &str = r#"
schema_version: 1
profiles:
  core: [ripgrep]
  recommended: [ripgrep, jq]
  full: [ripgrep, jq, absent-key]
formulas:
  ripgrep:
    brew: ripgrep
    command: rg
    categories: [search]
  jq:
    brew: jq
    command: jq
    categories: [json]
"#;

    fn skills_yaml(required_clis: &str) -> String {
        format!(
            r#"
schema_version: 1
skills:
  - id: sample.one
    domain: sample
    source: core/skills/sample
    products:
      codex:
        name: /sample-one
        render_to: skills/sample/SKILL.md
    required_clis:
{required_clis}
"#
        )
    }

    fn source_root(skills: &str, cli_tools: &str) -> TempDir {
        let tmp = TempDir::new().unwrap();
        let manifests = tmp.path().join("manifests");
        fs::create_dir_all(&manifests).unwrap();
        fs::write(manifests.join("skills.yaml"), skills).unwrap();
        fs::write(manifests.join("cli-tools.yaml"), cli_tools).unwrap();
        tmp
    }

    #[test]
    fn check_and_status_labels_are_the_published_wire_values() {
        assert_eq!(CoverageKind::RequiredCli.check(), "required-cli");
        assert_eq!(CoverageKind::CliTool.check(), "cli-tool");
        assert_eq!(CoverageStatus::Ok.as_str(), "ok");
        assert_eq!(CoverageStatus::Missing.as_str(), "missing");
        assert_eq!(CoverageStatus::Outdated.as_str(), "outdated");
        assert_eq!(CoverageStatus::Unparseable.as_str(), "unparseable");
    }

    #[test]
    fn a_finding_renders_every_populated_field_into_the_doctor_message() {
        let finding = CoverageFinding {
            kind: CoverageKind::RequiredCli,
            name: "agent-out".to_string(),
            command: "agent-out".to_string(),
            status: CoverageStatus::Outdated,
            severity: DoctorSeverity::Warn,
            required_version: Some(">=0.5.0".to_string()),
            parsed_version: Some("0.4.0".to_string()),
            formula: Some("nils-cli".to_string()),
            message: "below floor".to_string(),
        };

        let doctor = finding.to_doctor_finding("codex");

        assert_eq!(doctor.product, "codex");
        assert_eq!(doctor.check, "required-cli");
        assert_eq!(doctor.severity, DoctorSeverity::Warn);
        assert_eq!(doctor.entry_id.as_deref(), Some("agent-out"));
        assert_eq!(
            doctor.message,
            "status=outdated command=`agent-out` required=>=0.5.0 parsed=0.4.0 upgrade=`brew upgrade nils-cli`: below floor"
        );
    }

    #[test]
    fn severity_selects_the_doctor_finding_constructor() {
        let base = CoverageFinding {
            kind: CoverageKind::CliTool,
            name: "ripgrep".to_string(),
            command: "rg".to_string(),
            status: CoverageStatus::Ok,
            severity: DoctorSeverity::Ok,
            required_version: None,
            parsed_version: None,
            formula: None,
            message: String::new(),
        };

        let ok = base.to_doctor_finding("claude");
        assert_eq!(ok.severity, DoctorSeverity::Ok);
        // With every optional field absent the message stays minimal.
        assert_eq!(ok.message, "status=ok command=`rg`");

        let blocked = CoverageFinding {
            severity: DoctorSeverity::Block,
            status: CoverageStatus::Missing,
            ..base
        };
        let blocked = blocked.to_doctor_finding("claude");
        assert_eq!(blocked.severity, DoctorSeverity::Block);
        assert_eq!(blocked.check, "cli-tool");
    }

    #[test]
    fn a_missing_manifest_names_the_file_it_expected() {
        let tmp = TempDir::new().unwrap();
        let err = probe(tmp.path(), "core").expect_err("no manifests");

        match err {
            CoverageError::Missing { path } => {
                assert!(path.ends_with("manifests/skills.yaml"), "{path:?}");
            }
            other => panic!("expected Missing, got {other:?}"),
        }
    }

    #[test]
    fn an_unparseable_manifest_reports_a_parse_error() {
        let tmp = source_root("schema_version: 1\nskills: [\n", CLI_TOOLS);
        let err = probe(tmp.path(), "core").expect_err("broken yaml");

        assert!(
            matches!(err, CoverageError::Parse { .. }),
            "expected Parse, got {err:?}"
        );
        assert!(err.to_string().starts_with("parse error in"));
    }

    #[test]
    fn schema_version_drift_is_reported_per_manifest_family() {
        // skills.yaml accepts a set of versions, so drift lists all of them.
        let tmp = source_root("schema_version: 9\nskills: []\n", CLI_TOOLS);
        match probe(tmp.path(), "core").expect_err("skills drift") {
            CoverageError::SchemaVersions {
                expected, found, ..
            } => {
                assert_eq!(expected, SKILLS_SCHEMA_VERSIONS.to_vec());
                assert_eq!(found, 9);
            }
            other => panic!("expected SchemaVersions, got {other:?}"),
        }

        // cli-tools.yaml pins exactly one version.
        let tmp = source_root(
            "schema_version: 1\nskills: []\n",
            &CLI_TOOLS.replace("schema_version: 1", "schema_version: 7"),
        );
        match probe(tmp.path(), "core").expect_err("cli-tools drift") {
            CoverageError::SchemaVersion {
                expected, found, ..
            } => {
                assert_eq!(expected, SCHEMA_VERSION);
                assert_eq!(found, 7);
            }
            other => panic!("expected SchemaVersion, got {other:?}"),
        }
    }

    #[test]
    fn an_invalid_skills_contract_is_reported_before_any_probe() {
        // schema_version 1 with v2-only fields is a contract violation.
        let skills = r#"
schema_version: 1
skills:
  - id: sample.one
    domain: sample
    source: core/skills/sample
    invocation:
      role: workflow
      intents: [project-dev]
      example_request: "do the thing"
      admission_rationale: "because"
    products:
      codex:
        name: /sample-one
        render_to: skills/sample/SKILL.md
    required_clis: {}
"#;
        let tmp = source_root(skills, CLI_TOOLS);

        let err = probe(tmp.path(), "core").expect_err("invalid contract");

        assert!(
            matches!(err, CoverageError::InvalidSkills { .. }),
            "expected InvalidSkills, got {err:?}"
        );
    }

    #[test]
    fn an_unknown_profile_is_rejected_by_name() {
        let tmp = source_root("schema_version: 1\nskills: []\n", CLI_TOOLS);

        let err = probe(tmp.path(), "everything").expect_err("unknown profile");

        assert_eq!(
            err.to_string(),
            "unknown cli-tools profile `everything`; expected core, recommended, or full"
        );
    }

    #[test]
    fn each_profile_selects_its_own_tool_list() {
        let tmp = source_root("schema_version: 1\nskills: []\n", CLI_TOOLS);

        let core = probe(tmp.path(), "core").expect("core");
        let recommended = probe(tmp.path(), "recommended").expect("recommended");
        let full = probe(tmp.path(), "full").expect("full");

        assert_eq!(core.len(), 1);
        assert_eq!(recommended.len(), 2);
        assert_eq!(full.len(), 3);
        assert!(core.iter().all(|f| f.kind == CoverageKind::CliTool));
    }

    #[test]
    fn a_profile_key_without_a_formula_is_a_warning_not_a_hard_error() {
        let tmp = source_root("schema_version: 1\nskills: []\n", CLI_TOOLS);

        let findings = probe(tmp.path(), "full").expect("full profile");
        let orphan = findings
            .iter()
            .find(|f| f.name == "absent-key")
            .expect("orphan key finding");

        assert_eq!(orphan.status, CoverageStatus::Missing);
        assert_eq!(orphan.severity, DoctorSeverity::Warn);
        assert_eq!(orphan.formula, None);
        assert_eq!(
            orphan.message,
            "profile references a formula key that is not declared"
        );
    }

    #[test]
    fn a_required_cli_that_is_not_on_path_blocks() {
        let tmp = source_root(
            &skills_yaml("      nils-definitely-absent-binary: \">=1.0.0\""),
            CLI_TOOLS,
        );

        let findings = probe(tmp.path(), "core").expect("probe");
        let required = findings
            .iter()
            .find(|f| f.kind == CoverageKind::RequiredCli)
            .expect("required-cli finding");

        assert_eq!(required.name, "nils-definitely-absent-binary");
        assert_eq!(required.status, CoverageStatus::Missing);
        assert_eq!(required.severity, DoctorSeverity::Block);
        assert_eq!(required.required_version.as_deref(), Some(">=1.0.0"));
        assert_eq!(required.formula.as_deref(), Some("nils-cli"));
    }

    #[test]
    fn the_highest_declared_floor_wins_across_skills() {
        let skills = r#"
schema_version: 1
skills:
  - id: sample.low
    domain: sample
    source: core/skills/low
    products:
      codex:
        name: /low
        render_to: skills/sample/LOW.md
    required_clis:
      agent-out: "0.5.0"
  - id: sample.high
    domain: sample
    source: core/skills/high
    products:
      codex:
        name: /high
        render_to: skills/sample/HIGH.md
    required_clis:
      agent-out: "1.2.0"
"#;
        let manifest: SkillsManifest = serde_yaml_ng::from_str(skills).expect("skills");

        let floors = required_cli_floors(&manifest);

        assert_eq!(floors.len(), 1);
        assert_eq!(floors.get("agent-out").map(String::as_str), Some("1.2.0"));
    }

    #[test]
    fn floors_that_cannot_be_parsed_fall_back_to_lexical_order() {
        assert!(floor_cmp("1.2.0", "1.10.0").is_lt(), "semver, not lexical");
        assert!(floor_cmp("1.10.0", "1.2.0").is_gt());
        assert!(floor_cmp("nightly", "stable").is_lt());
        assert!(floor_cmp("1.0.0", "1.0.0").is_eq());
    }

    #[test]
    fn an_explicit_path_is_probed_directly_instead_of_searching_path() {
        let tmp = TempDir::new().unwrap();
        let script = tmp.path().join("bin").join("tool");
        fs::create_dir_all(script.parent().unwrap()).unwrap();
        fs::write(&script, "#!/bin/sh\nexit 0\n").unwrap();

        // A path-shaped program name is never looked up in PATH; before the
        // executable bit is set it must not resolve.
        let name = script.to_string_lossy().to_string();
        assert_eq!(find_in_path(&name), None);
        assert!(!is_executable_file(&script));

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(&script, fs::Permissions::from_mode(0o755)).unwrap();
            assert_eq!(find_in_path(&name), Some(script.clone()));
            assert!(is_executable_file(&script));
        }

        // A directory and a missing path are never executables.
        assert!(!is_executable_file(tmp.path()));
        assert!(!is_executable_file(&tmp.path().join("absent")));
        assert_eq!(find_in_path("nils-definitely-absent-binary"), None);
    }

    #[test]
    fn windows_pathext_defaults_are_normalized_to_dotted_extensions() {
        let extensions = windows_pathext_extensions();

        assert!(
            extensions
                .iter()
                .all(|ext| ext.to_string_lossy().starts_with('.')),
            "every extension must be dotted: {extensions:?}"
        );
        assert!(!extensions.is_empty());
    }

    #[test]
    fn brew_probe_is_inert_when_homebrew_is_absent() {
        // Whatever the host looks like, the probe must return a bool rather
        // than panicking or shelling out unconditionally.
        let _ = brew_reports_outdated("definitely-not-a-formula");
    }
}

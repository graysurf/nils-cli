//! Copy-pasteable Homebrew upgrade suggestions for doctor findings.

use super::coverage::CoverageStatus;
use super::version::VersionStatus;
use super::{DoctorOutcome, DoctorSeverity};
use std::collections::BTreeSet;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UpgradeSuggestion {
    pub formula: String,
    pub command: String,
}

pub fn suggestions(outcome: &DoctorOutcome) -> Vec<UpgradeSuggestion> {
    let mut formulas = BTreeSet::new();
    for probe in &outcome.version_probes {
        if probe.status != VersionStatus::Ok {
            formulas.insert(probe.product.clone());
        }
    }
    for probe in &outcome.coverage_probes {
        if probe.severity == DoctorSeverity::Ok || probe.status == CoverageStatus::Ok {
            continue;
        }
        if let Some(formula) = probe.formula.as_deref() {
            formulas.insert(formula.to_string());
        }
    }
    formulas
        .into_iter()
        .map(|formula| UpgradeSuggestion {
            command: format!("brew upgrade {formula}"),
            formula,
        })
        .collect()
}

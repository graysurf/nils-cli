//! `agent-runtime doctor --class installed-runtime` receipt verification.

use std::path::Path;
use std::time::SystemTime;

use serde::Serialize;

use crate::doctor::DoctorFinding;
use crate::install::InstallPlan;
use crate::install::receipt::{self, InstallReceipt, RECEIPT_SCHEMA};

#[derive(Debug, Clone, Serialize)]
pub struct InstalledRuntimeReport {
    pub receipt_present: bool,
    pub verified: bool,
    pub source_clean: bool,
    pub source_match: bool,
    pub plan_match: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub receipt: Option<InstallReceipt>,
    pub findings: Vec<DoctorFinding>,
}

#[derive(Debug, thiserror::Error)]
pub enum InstalledRuntimeError {
    #[error("failed to compute current install provenance: {0}")]
    Current(#[from] receipt::ReceiptError),
}

pub fn check(
    product: &str,
    plan: &InstallPlan,
    focused: bool,
) -> Result<InstalledRuntimeReport, InstalledRuntimeError> {
    let current = receipt::build(plan, SystemTime::UNIX_EPOCH)?;
    let stored = match receipt::read(&plan.state_home, product) {
        Ok(receipt) => Some(receipt),
        Err(receipt::ReceiptError::Read(_)) => None,
        Err(err) => return Err(err.into()),
    };
    let mut findings = Vec::new();
    let Some(stored) = stored else {
        findings.push(if focused {
            DoctorFinding::block(
                product,
                "installed-runtime-receipt",
                None,
                Some(receipt::path(&plan.state_home, product)),
                "portable install receipt is missing; run agent-runtime install --apply",
            )
        } else {
            DoctorFinding::warn(
                product,
                "installed-runtime-receipt",
                None,
                Some(receipt::path(&plan.state_home, product)),
                "portable install receipt is missing from an older install",
            )
        });
        return Ok(InstalledRuntimeReport {
            receipt_present: false,
            verified: false,
            source_clean: false,
            source_match: false,
            plan_match: false,
            receipt: None,
            findings,
        });
    };
    if stored.schema != RECEIPT_SCHEMA || stored.product != product {
        findings.push(DoctorFinding::block(
            product,
            "installed-runtime-receipt",
            None,
            Some(receipt::path(&plan.state_home, product)),
            "install receipt schema or product does not match the requested runtime",
        ));
    }
    let source_clean = !stored.source_dirty && stored.source_revision != "unavailable";
    if focused && !source_clean {
        findings.push(DoctorFinding::block(
            product,
            "installed-runtime-source-clean",
            None,
            None,
            "focused acceptance requires a receipt produced from a clean Git revision",
        ));
    }
    let source_match = stored.source_revision == current.source_revision
        && stored.source_dirty == current.source_dirty;
    if !source_match {
        findings.push(DoctorFinding::block(
            product,
            "installed-runtime-source",
            None,
            None,
            "installed source provenance differs from the supplied source checkout",
        ));
    }
    let plan_match = stored.install_plan_digest == current.install_plan_digest
        && stored.managed_entries == current.managed_entries;
    if !plan_match {
        findings.push(DoctorFinding::block(
            product,
            "installed-runtime-plan",
            None,
            None,
            "installed plan/content digest differs from the supplied install plan",
        ));
    }
    Ok(InstalledRuntimeReport {
        receipt_present: true,
        verified: findings.is_empty(),
        source_clean,
        source_match,
        plan_match,
        receipt: Some(stored),
        findings,
    })
}

#[allow(dead_code)]
fn _path_is_private(_: &Path) {}

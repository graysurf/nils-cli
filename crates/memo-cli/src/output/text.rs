use crate::output::format_item_id;
use crate::storage::derivations::ApplySummary;
use crate::storage::repository::{FetchItem, ListItem};
use crate::storage::search::{ReportSummary, SearchItem};
use nils_common::env as shared_env;

pub fn print_add(item_id: i64, created_at: &str) {
    println!("added {} at {}", format_item_id(item_id), created_at);
}

pub fn print_update(
    item_id: i64,
    updated_at: &str,
    cleared_derivations: i64,
    cleared_workflow_anchors: i64,
) {
    println!(
        "updated {} at {} (state=pending, cleared_derivations={}, cleared_workflows={})",
        format_item_id(item_id),
        updated_at,
        cleared_derivations,
        cleared_workflow_anchors
    );
}

pub fn print_delete(
    item_id: i64,
    deleted_at: &str,
    removed_derivations: i64,
    removed_workflow_anchors: i64,
) {
    println!(
        "deleted {} at {} (removed_derivations={}, removed_workflows={})",
        format_item_id(item_id),
        deleted_at,
        removed_derivations,
        removed_workflow_anchors
    );
}

pub fn print_list(rows: &[ListItem], limit: usize, offset: usize, truncated: bool) {
    if rows.is_empty() {
        println!("(no items)");
        return;
    }

    println!(
        "{}\t{}\t{}\t{}",
        style_heading("item_id"),
        style_heading("created_at"),
        style_heading("state"),
        style_heading("preview")
    );
    for row in rows {
        println!(
            "{}\t{}\t{}\t{}",
            format_item_id(row.item_id),
            row.created_at,
            style_state(&row.state),
            row.text_preview
        );
    }

    if truncated {
        println!(
            "(showing {} items with --limit {limit}; use --limit <N> to show more or --offset {} for the next page)",
            rows.len(),
            offset.saturating_add(limit),
        );
    }
}

pub fn print_search(rows: &[SearchItem], limit: usize, truncated: bool) {
    if rows.is_empty() {
        println!("(no matches)");
        return;
    }

    println!(
        "{}\t{}\t{}\t{}",
        style_heading("item_id"),
        style_heading("created_at"),
        style_heading("score"),
        style_heading("preview")
    );
    for row in rows {
        println!(
            "{}\t{}\t{:.4}\t{}",
            format_item_id(row.item_id),
            row.created_at,
            row.score,
            row.preview
        );
    }

    if truncated {
        println!(
            "(showing {} matches with --limit {limit}; use --limit <N> to show more)",
            rows.len(),
        );
    }
}

pub fn print_report(summary: &ReportSummary) {
    println!("report: {}", summary.period);
    println!(
        "range: {} .. {} ({})",
        summary.range.from, summary.range.to, summary.range.timezone
    );
    println!("captured: {}", summary.totals.captured);
    println!("enriched: {}", summary.totals.enriched);
    println!("pending: {}", summary.totals.pending);

    if !summary.top_categories.is_empty() {
        println!("top categories:");
        for item in &summary.top_categories {
            println!("  - {} ({})", item.name, item.count);
        }
    }

    if !summary.top_tags.is_empty() {
        println!("top tags:");
        for item in &summary.top_tags {
            println!("  - {} ({})", item.name, item.count);
        }
    }
}

pub fn print_fetch(rows: &[FetchItem]) {
    println!("pending items: {}", rows.len());
    for row in rows {
        println!(
            "{}\t{}\t{}\t{}",
            format_item_id(row.item_id),
            row.created_at,
            row.source,
            row.text
        );
    }
}

pub fn print_apply(summary: &ApplySummary) {
    println!(
        "apply payload processed={} accepted={} skipped={} failed={} dry_run={}",
        summary.processed, summary.accepted, summary.skipped, summary.failed, summary.dry_run
    );

    if summary.dry_run && !summary.changes.is_empty() {
        println!("changes:");
        for change in summary.changes.iter().take(10) {
            println!(
                "  - {} {}: {} -> {}",
                format_item_id(change.item_id),
                change.field,
                change
                    .old
                    .as_ref()
                    .map_or_else(|| "<none>".to_string(), serde_json::Value::to_string),
                change.new
            );
        }
        if summary.changes.len() > 10 {
            println!("  +{} more", summary.changes.len() - 10);
        }
    }

    for item in &summary.items {
        if let Some(error) = &item.error {
            eprintln!(
                "warning: {} {}: {}",
                format_item_id(item.item_id),
                item.status,
                error.message
            );
        }
    }
}

fn style_heading(label: &str) -> String {
    if color_enabled() {
        format!("\u{1b}[1m{label}\u{1b}[0m")
    } else {
        label.to_string()
    }
}

fn style_state(state: &str) -> String {
    if !color_enabled() {
        return state.to_string();
    }

    match state {
        "pending" => format!("\u{1b}[33m{state}\u{1b}[0m"),
        "enriched" => format!("\u{1b}[32m{state}\u{1b}[0m"),
        _ => state.to_string(),
    }
}

fn color_enabled() -> bool {
    // Keep memo-cli's existing behavior: empty NO_COLOR still keeps color enabled.
    !shared_env::no_color_non_empty_enabled()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::storage::derivations::{ApplyItemError, ApplyItemOutcome};
    use crate::storage::search::{NameCount, ReportRange, ReportTotals};
    use nils_test_support::{EnvGuard, GlobalStateLock};

    fn sample_list_rows() -> Vec<ListItem> {
        vec![
            ListItem {
                item_id: 3,
                created_at: "2026-02-12T10:00:00Z".to_string(),
                state: "pending".to_string(),
                text_preview: "plan sprint".to_string(),
                content_type: None,
                validation_status: None,
            },
            ListItem {
                item_id: 2,
                created_at: "2026-02-12T09:00:00Z".to_string(),
                state: "enriched".to_string(),
                text_preview: "book dentist".to_string(),
                content_type: Some("text".to_string()),
                validation_status: Some("valid".to_string()),
            },
            ListItem {
                item_id: 1,
                created_at: "2026-02-12T08:00:00Z".to_string(),
                state: "archived".to_string(),
                text_preview: "archived note".to_string(),
                content_type: None,
                validation_status: None,
            },
        ]
    }

    fn sample_search_rows() -> Vec<SearchItem> {
        vec![SearchItem {
            item_id: 7,
            created_at: "2026-02-11T10:00:00Z".to_string(),
            score: -0.1203,
            matched_fields: vec!["raw_text".to_string()],
            preview: "tokyo travel event".to_string(),
            content_type: Some("text".to_string()),
            validation_status: Some("valid".to_string()),
        }]
    }

    fn sample_report() -> ReportSummary {
        ReportSummary {
            period: "week".to_string(),
            range: ReportRange {
                from: "2026-02-05T00:00:00Z".to_string(),
                to: "2026-02-12T00:00:00Z".to_string(),
                timezone: "UTC".to_string(),
            },
            totals: ReportTotals {
                captured: 5,
                enriched: 4,
                pending: 1,
            },
            top_categories: vec![NameCount {
                name: "travel".to_string(),
                count: 2,
            }],
            top_tags: vec![NameCount {
                name: "family".to_string(),
                count: 3,
            }],
            top_content_types: vec![NameCount {
                name: "text".to_string(),
                count: 5,
            }],
            validation_status_totals: vec![NameCount {
                name: "valid".to_string(),
                count: 4,
            }],
        }
    }

    #[test]
    fn print_text_output_paths_are_exercised() {
        print_add(1, "2026-02-12T10:00:00Z");
        print_update(1, "2026-02-12T10:30:00Z", 2, 1);
        print_delete(1, "2026-02-12T10:40:00Z", 2, 1);
        print_list(&[], 20, 0, false);
        print_list(&sample_list_rows(), 20, 0, false);

        print_search(&[], 20, false);
        print_search(&sample_search_rows(), 20, false);

        print_report(&sample_report());
        print_report(&ReportSummary {
            top_categories: Vec::new(),
            top_tags: Vec::new(),
            ..sample_report()
        });

        print_fetch(&[]);
        print_fetch(&[FetchItem {
            item_id: 9,
            created_at: "2026-02-12T11:00:00Z".to_string(),
            source: "cli".to_string(),
            text: "renew passport in april".to_string(),
            state: "pending".to_string(),
            content_type: None,
            validation_status: None,
        }]);

        print_apply(&ApplySummary {
            dry_run: false,
            processed: 2,
            accepted: 1,
            skipped: 0,
            failed: 1,
            changes: Vec::new(),
            items: vec![
                ApplyItemOutcome {
                    item_id: 9,
                    status: "accepted".to_string(),
                    derivation_version: Some(1),
                    content_type: Some("text".to_string()),
                    validation_status: Some("valid".to_string()),
                    validation_errors: None,
                    error: None,
                },
                ApplyItemOutcome {
                    item_id: 8,
                    status: "failed".to_string(),
                    derivation_version: None,
                    content_type: None,
                    validation_status: None,
                    validation_errors: None,
                    error: Some(ApplyItemError {
                        code: "invalid-apply-payload".to_string(),
                        message: "item_id does not exist".to_string(),
                        details: None,
                    }),
                },
            ],
        });
    }

    #[test]
    fn style_helpers_cover_color_and_no_color_modes() {
        let lock = GlobalStateLock::new();
        let _baseline = EnvGuard::remove(&lock, "NO_COLOR");

        {
            let _no_color = EnvGuard::set(&lock, "NO_COLOR", "1");
            assert_eq!(style_heading("item_id"), "item_id");
            assert_eq!(style_state("pending"), "pending");
        }

        {
            let _empty_no_color = EnvGuard::set(&lock, "NO_COLOR", "");
            let heading = style_heading("item_id");
            assert!(heading.contains("item_id"));
            assert!(heading.contains('\u{1b}'));
            let pending = style_state("pending");
            assert!(pending.contains('\u{1b}'));
            let enriched = style_state("enriched");
            assert!(enriched.contains('\u{1b}'));
            assert_eq!(style_state("unknown"), "unknown");
        }
    }
}

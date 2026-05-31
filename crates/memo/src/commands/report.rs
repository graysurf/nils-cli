use chrono::{Datelike, Duration, TimeZone, Utc};
use serde_json::json;

use crate::cli::{OutputFormat, ReportArgs, ReportPeriod};
use crate::errors::AppError;
use crate::output::{emit_data, text};
use crate::storage::Storage;
use crate::storage::search::{self, ReportRangeQuery};
use crate::timestamps::{format_utc, parse_rfc3339_utc, parse_timezone};

pub fn run(storage: &Storage, format: OutputFormat, args: &ReportArgs) -> Result<(), AppError> {
    let query = resolve_report_range(args)?;
    let summary =
        storage.with_connection(|conn| search::report_summary_with_range(conn, &query))?;

    if format.is_json() {
        return emit_data(
            "cli.memo.report.v1",
            json!({
                "period": summary.period,
                "range": summary.range,
                "totals": summary.totals,
                "top_categories": summary.top_categories,
                "top_tags": summary.top_tags,
                "top_content_types": summary.top_content_types,
                "validation_status_totals": summary.validation_status_totals,
            }),
        );
    }

    text::print_report(&summary);

    Ok(())
}

fn resolve_report_range(args: &ReportArgs) -> Result<ReportRangeQuery, AppError> {
    let tz_name = args.tz.clone().unwrap_or_else(|| "UTC".to_string());
    let tz = parse_timezone(&tz_name)?;
    let period = period_label(args.period).to_string();

    match (args.from.as_deref(), args.to.as_deref()) {
        (Some(from_raw), Some(to_raw)) => {
            let from = parse_rfc3339_utc(from_raw, "--from")?;
            let to = parse_rfc3339_utc(to_raw, "--to")?;
            if from > to {
                return Err(AppError::usage("--from must be less than or equal to --to")
                    .with_code("invalid-time-range"));
            }

            Ok(ReportRangeQuery {
                period,
                from: format_utc(from),
                to: format_utc(to),
                timezone: tz_name,
            })
        }
        (None, None) => {
            let now_tz = Utc::now().with_timezone(&tz);
            let from_tz = match args.period {
                ReportPeriod::Week => now_tz - Duration::days(7),
                ReportPeriod::Month => tz
                    .with_ymd_and_hms(now_tz.year(), now_tz.month(), 1, 0, 0, 0)
                    .single()
                    .ok_or_else(|| {
                        AppError::runtime("failed to calculate month boundary for timezone")
                            .with_code("invalid-timezone")
                    })?,
            };

            Ok(ReportRangeQuery {
                period,
                from: format_utc(from_tz.with_timezone(&Utc)),
                to: format_utc(now_tz.with_timezone(&Utc)),
                timezone: tz_name,
            })
        }
        _ => Err(AppError::usage("--from and --to must be provided together")
            .with_code("invalid-arguments")),
    }
}

fn period_label(period: ReportPeriod) -> &'static str {
    match period {
        ReportPeriod::Week => "week",
        ReportPeriod::Month => "month",
    }
}

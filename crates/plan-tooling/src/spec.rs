use std::io::Write;

use serde::Serialize;

use crate::validate::EXPLAIN_CATALOG;

const USAGE: &str = r#"Usage:
  plan-tooling spec [--format text|json]

Purpose:
  Dump the validate catalog (every error class, the matching pattern, the
  authoring rule, and a canonical example) as a stable, machine-readable
  surface. Use this instead of grepping the binary for validation rules.

Options:
  --format <fmt>   text (default) or json
  -h, --help       Show help
  -V, --version    Show plan-tooling version

Exit:
  0: spec printed successfully
  2: usage error
"#;

fn print_usage() {
    let _ = std::io::stderr().write_all(USAGE.as_bytes());
}

fn die(msg: &str) -> i32 {
    eprintln!("plan-tooling spec: {msg}");
    2
}

/// One entry in the spec output. Field order is kept stable across runs so
/// `plan-tooling spec --format json` is safe to diff between binary versions.
#[derive(Debug, Clone, Serialize)]
struct SpecEntry {
    class: &'static str,
    pattern: &'static str,
    rule: &'static str,
    example: &'static str,
}

pub fn run(args: &[String]) -> i32 {
    let mut format = "text".to_string();
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--format" => {
                let Some(value) = args.get(i + 1) else {
                    return die("--format requires a value");
                };
                format = value.to_string();
                i += 2;
            }
            "-h" | "--help" => {
                print_usage();
                return 0;
            }
            "-V" | "--version" => {
                crate::usage::print_version_stdout();
                return 0;
            }
            other => return die(&format!("unknown argument: {other}")),
        }
    }

    if format != "text" && format != "json" {
        return die(&format!("invalid --format (expected text|json): {format}"));
    }

    let mut entries: Vec<SpecEntry> = EXPLAIN_CATALOG
        .iter()
        .map(|entry| SpecEntry {
            class: entry.explain.class,
            pattern: entry.pattern,
            rule: entry.explain.rule,
            example: entry.explain.example,
        })
        .collect();
    // Stable order regardless of catalog declaration order.
    entries.sort_by_key(|e| e.class);

    match format.as_str() {
        "json" => match serde_json::to_string(&entries) {
            Ok(s) => {
                println!("{s}");
                0
            }
            Err(err) => {
                eprintln!("error: failed to encode JSON: {err}");
                1
            }
        },
        "text" => {
            print_text(&entries);
            0
        }
        _ => unreachable!(),
    }
}

fn print_text(entries: &[SpecEntry]) {
    for (idx, entry) in entries.iter().enumerate() {
        if idx > 0 {
            println!();
        }
        println!("[{}]", entry.class);
        println!("  pattern: {}", entry.pattern);
        println!("  rule:    {}", entry.rule);
        println!("  example:");
        for line in entry.example.lines() {
            println!("      {line}");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::SpecEntry;
    use crate::validate::EXPLAIN_CATALOG;
    use pretty_assertions::assert_eq;

    #[test]
    fn spec_entries_sort_by_class_for_stability() {
        let mut entries: Vec<SpecEntry> = EXPLAIN_CATALOG
            .iter()
            .map(|entry| SpecEntry {
                class: entry.explain.class,
                pattern: entry.pattern,
                rule: entry.explain.rule,
                example: entry.explain.example,
            })
            .collect();
        entries.sort_by_key(|e| e.class);

        let classes: Vec<&str> = entries.iter().map(|e| e.class).collect();
        let mut sorted = classes.clone();
        sorted.sort();
        assert_eq!(classes, sorted, "spec entries must be sorted by class");
    }

    #[test]
    fn spec_emits_a_classified_entry_per_catalog_row() {
        let entries: Vec<SpecEntry> = EXPLAIN_CATALOG
            .iter()
            .map(|entry| SpecEntry {
                class: entry.explain.class,
                pattern: entry.pattern,
                rule: entry.explain.rule,
                example: entry.explain.example,
            })
            .collect();
        assert_eq!(entries.len(), EXPLAIN_CATALOG.len());
        for entry in &entries {
            assert!(!entry.class.is_empty());
            assert!(!entry.pattern.is_empty());
            assert!(!entry.rule.is_empty());
            assert!(!entry.example.is_empty());
        }
    }
}

pub fn print_help() {
    println!("Usage: git-summary <command> [args]");
    println!();
    println!("Commands:");
    println!("  {:<18}  Entire history", "all");
    println!("  {:<18}  Today only", "today");
    println!("  {:<18}  Yesterday only", "yesterday");
    println!("  {:<18}  1st to today", "this-month");
    println!("  {:<18}  1st to end of last month", "last-month");
    println!("  {:<18}  This Mon–Sun", "this-week");
    println!("  {:<18}  Last Mon–Sun", "last-week");
    println!(
        "  {:<18}  Export shell completion script",
        "completion <shell>"
    );
    println!("  {:<18}  Show this help", "help");
    println!("  {:<18}  Custom date range (YYYY-MM-DD)", "<from> <to>");
    println!();
}

pub fn print_header(label: &str) {
    println!();
    println!("📅 Git summary for {label}");
    println!();
}

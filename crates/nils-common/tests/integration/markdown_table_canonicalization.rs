use nils_common::markdown::canonicalize_table_cell;

#[test]
fn canonicalize_table_cell_normalizes_table_breaking_chars() {
    assert_eq!(
        canonicalize_table_cell("alpha|beta\r\ngamma\ndelta\rzeta"),
        "alpha/beta gamma delta zeta"
    );
}

#[test]
fn canonicalize_table_cell_is_idempotent_for_round_trips() {
    let once = canonicalize_table_cell("A|B\nC");
    let twice = canonicalize_table_cell(&once);
    assert_eq!(once, twice);
}

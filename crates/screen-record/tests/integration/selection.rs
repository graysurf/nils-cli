use screen_record::select::{SelectionArgs, select_window};
use screen_record::types::{Rect, WindowInfo};

fn window(
    id: u32,
    owner: &str,
    title: &str,
    on_screen: bool,
    active: bool,
    z: usize,
) -> WindowInfo {
    WindowInfo {
        id,
        owner_name: owner.to_string(),
        title: title.to_string(),
        bounds: Rect {
            x: 0,
            y: 0,
            width: 100,
            height: 100,
        },
        on_screen,
        active,
        owner_pid: 1,
        z_order: z,
    }
}

#[test]
fn ambiguous_app_selection_outputs_candidates() {
    let windows = vec![
        window(10, "Terminal", "Inbox", false, false, 0),
        window(11, "Terminal", "Docs", false, false, 1),
    ];
    let args = SelectionArgs {
        app: Some("Terminal".to_string()),
        ..SelectionArgs::default()
    };

    let err = select_window(&windows, &args).expect_err("ambiguous error");
    let message = err.to_string();
    assert!(message.contains("multiple windows match --app \"Terminal\""));
    assert!(message.contains("Terminal\tDocs"));
    assert!(message.contains("Terminal\tInbox"));
    assert_eq!(err.exit_code(), 2);
}

#[test]
fn missing_selection_flag_errors() {
    let windows = vec![window(10, "Terminal", "Inbox", true, false, 0)];
    let args = SelectionArgs::default();
    let err = select_window(&windows, &args).expect_err("missing selection");
    assert_eq!(err.exit_code(), 2);
    assert!(err.to_string().contains("missing selection flag"));
}

#[test]
fn select_by_window_id_missing_errors() {
    let windows = vec![window(10, "Terminal", "Inbox", true, false, 0)];
    let args = SelectionArgs {
        window_id: Some(99),
        ..SelectionArgs::default()
    };
    let err = select_window(&windows, &args).expect_err("missing window");
    assert_eq!(err.exit_code(), 2);
    assert!(err.to_string().contains("no window found with id 99"));
}

#[test]
fn select_active_window_missing_errors() {
    let windows = vec![window(10, "Terminal", "Inbox", false, false, 0)];
    let args = SelectionArgs {
        active_window: true,
        ..SelectionArgs::default()
    };
    let err = select_window(&windows, &args).expect_err("no active window");
    assert_eq!(err.exit_code(), 2);
    assert!(err.to_string().contains("no active window found"));
}

#[test]
fn select_by_app_no_match_errors() {
    let windows = vec![window(10, "Terminal", "Inbox", true, false, 0)];
    let args = SelectionArgs {
        app: Some("Safari".to_string()),
        ..SelectionArgs::default()
    };
    let err = select_window(&windows, &args).expect_err("no matching app");
    assert_eq!(err.exit_code(), 2);
    assert!(
        err.to_string()
            .contains("no windows match --app \"Safari\"")
    );
}

#[test]
fn select_by_app_and_window_name_ambiguous_with_tied_z_order() {
    let windows = vec![
        window(10, "Terminal", "Inbox", true, true, 0),
        window(11, "Terminal", "Inbox (copy)", true, true, 0),
    ];
    let args = SelectionArgs {
        app: Some("Terminal".to_string()),
        window_name: Some("Inbox".to_string()),
        ..SelectionArgs::default()
    };

    let err = select_window(&windows, &args).expect_err("ambiguous due tied z-order");
    assert_eq!(err.exit_code(), 2);
    let message = err.to_string();
    assert!(message.contains("multiple windows match --app \"Terminal\""));
    assert!(message.contains("refine with --window-name or use --window-id"));
}

#[test]
fn format_window_tsv_normalizes_fields() {
    let mut win = window(10, "Te\tst", "Do\ncs", true, false, 0);
    win.owner_name = "Te\tst".to_string();
    win.title = "Do\ncs".to_string();
    let line = screen_record::select::format_window_tsv(&win);
    assert!(line.contains("Te st"));
    assert!(line.contains("Do cs"));
}

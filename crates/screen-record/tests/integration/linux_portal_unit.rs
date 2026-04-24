#[cfg(target_os = "linux")]
mod linux_portal_unit {
    use std::collections::HashMap;

    use nils_test_support::{EnvGuard, GlobalStateLock};
    use screen_record::linux::portal;

    #[test]
    fn portal_missing_error_text_is_actionable_and_deterministic() {
        let lock = GlobalStateLock::new();

        let _force_missing = EnvGuard::set(&lock, "AGENTS_SCREEN_RECORD_PORTAL_FORCE_MISSING", "1");
        let _force_available =
            EnvGuard::remove(&lock, "AGENTS_SCREEN_RECORD_PORTAL_FORCE_AVAILABLE");

        let err = portal::ensure_portal_available().expect_err("expected missing portal error");
        let message = err.to_string();
        assert!(message.contains("xdg-desktop-portal"));
        assert!(message.contains("Wayland-only"));
        assert!(message.contains("org.freedesktop.portal.Desktop"));
    }

    #[test]
    fn portal_acquire_test_mode_bypasses_dbus_and_returns_fixed_node_id() {
        let lock = GlobalStateLock::new();

        let _test_mode = EnvGuard::set(&lock, "AGENTS_SCREEN_RECORD_TEST_MODE", "1");
        let _force_missing = EnvGuard::remove(&lock, "AGENTS_SCREEN_RECORD_PORTAL_FORCE_MISSING");
        let _force_available =
            EnvGuard::remove(&lock, "AGENTS_SCREEN_RECORD_PORTAL_FORCE_AVAILABLE");

        let _dbus = EnvGuard::set(
            &lock,
            "DBUS_SESSION_BUS_ADDRESS",
            "unix:path=/does-not-exist",
        );

        let node_id = portal::acquire_pipewire_node_id().expect("node id");
        assert_eq!(node_id, portal::TEST_PIPEWIRE_NODE_ID);
    }

    #[test]
    fn portal_force_available_truthy_values_override_force_missing() {
        let lock = GlobalStateLock::new();

        for value in ["1", "true", " yes ", "On"] {
            let _force_available =
                EnvGuard::set(&lock, "AGENTS_SCREEN_RECORD_PORTAL_FORCE_AVAILABLE", value);
            let _force_missing =
                EnvGuard::set(&lock, "AGENTS_SCREEN_RECORD_PORTAL_FORCE_MISSING", "1");

            portal::ensure_portal_available().expect("truthy force available should win");
        }
    }

    #[test]
    fn portal_force_available_falsey_values_do_not_override_force_missing() {
        let lock = GlobalStateLock::new();

        for value in ["0", "false", "no", "off", "", "  ", "enabled"] {
            let _force_available =
                EnvGuard::set(&lock, "AGENTS_SCREEN_RECORD_PORTAL_FORCE_AVAILABLE", value);
            let _force_missing =
                EnvGuard::set(&lock, "AGENTS_SCREEN_RECORD_PORTAL_FORCE_MISSING", "1");

            let err = portal::ensure_portal_available()
                .expect_err("falsey value should not force available");
            let message = err.to_string();
            assert!(message.contains("xdg-desktop-portal"), "value={value}");
        }
    }

    #[test]
    fn portal_without_session_bus_reports_actionable_runtime_error() {
        let lock = GlobalStateLock::new();

        let _force_available =
            EnvGuard::remove(&lock, "AGENTS_SCREEN_RECORD_PORTAL_FORCE_AVAILABLE");
        let _force_missing = EnvGuard::remove(&lock, "AGENTS_SCREEN_RECORD_PORTAL_FORCE_MISSING");
        let _dbus = EnvGuard::set(
            &lock,
            "DBUS_SESSION_BUS_ADDRESS",
            "unix:path=/definitely-missing-agent-screen-record-portal.sock",
        );

        let err =
            portal::ensure_portal_available().expect_err("expected DBus session connect failure");
        let message = err.to_string();
        assert!(message.contains("Wayland-only session detected"));
        assert!(message.contains("DBUS_SESSION_BUS_ADDRESS is unset"));
    }

    #[test]
    fn parse_session_handle_reports_missing_key_and_type_mismatch() {
        let missing = HashMap::new();
        let err = portal::parse_session_handle_from_results(&missing).expect_err("missing key");
        assert_eq!(err, "missing key session_handle");

        let mut wrong_type = HashMap::new();
        wrong_type.insert(
            "session_handle".to_string(),
            zbus::zvariant::OwnedValue::from(7u32),
        );
        let err =
            portal::parse_session_handle_from_results(&wrong_type).expect_err("type mismatch");
        assert_eq!(err, "key session_handle has unexpected type");
    }

    #[test]
    fn parse_streams_reports_missing_key_and_type_mismatch() {
        let missing = HashMap::new();
        let err = portal::parse_streams_from_results(&missing).expect_err("missing key");
        assert_eq!(err, "missing key streams");

        let mut wrong_type = HashMap::new();
        wrong_type.insert(
            "streams".to_string(),
            zbus::zvariant::OwnedValue::from(7u32),
        );
        let err = portal::parse_streams_from_results(&wrong_type).expect_err("type mismatch");
        assert_eq!(err, "key streams has unexpected type");
    }

    #[test]
    fn parse_session_handle_and_streams_accept_valid_values() {
        let session_path = zbus::zvariant::ObjectPath::try_from(
            "/org/freedesktop/portal/desktop/session/1_42/screen_record",
        )
        .expect("session path");
        let mut session_dict = HashMap::new();
        session_dict.insert(
            "session_handle".to_string(),
            zbus::zvariant::OwnedValue::from(session_path.clone()),
        );

        let parsed_session =
            portal::parse_session_handle_from_results(&session_dict).expect("parse session");
        assert_eq!(parsed_session.to_string(), session_path.to_string());

        let mut stream_meta = HashMap::new();
        stream_meta.insert(
            "source_type".to_string(),
            zbus::zvariant::OwnedValue::from(1u32),
        );
        let expected_streams = vec![(4242u32, stream_meta)];
        let mut stream_dict = HashMap::new();
        stream_dict.insert(
            "streams".to_string(),
            zbus::zvariant::OwnedValue::try_from(zbus::zvariant::Value::from(
                expected_streams.clone(),
            ))
            .expect("streams value"),
        );

        let streams = portal::parse_streams_from_results(&stream_dict).expect("parse streams");
        assert_eq!(streams.len(), 1);
        assert_eq!(streams[0].0, 4242);
        assert_eq!(streams[0].1.len(), 1);
        assert!(streams[0].1.contains_key("source_type"));
    }

    #[test]
    fn ensure_response_code_ok_rejects_non_zero_portal_response_code() {
        let err = portal::ensure_response_code_ok(2, HashMap::new())
            .expect_err("non-zero response must be rejected");
        let message = err.to_string();
        assert!(message.contains("portal request failed or was cancelled"));
        assert!(message.contains("response=2"));
    }

    #[test]
    fn ensure_response_code_ok_accepts_zero_and_preserves_results() {
        let mut results = HashMap::new();
        results.insert(
            "kind".to_string(),
            zbus::zvariant::OwnedValue::from(zbus::zvariant::Str::from("ok")),
        );

        let out = portal::ensure_response_code_ok(0, results).expect("zero response should pass");
        assert_eq!(out.len(), 1);
        assert!(out.contains_key("kind"));
    }
}

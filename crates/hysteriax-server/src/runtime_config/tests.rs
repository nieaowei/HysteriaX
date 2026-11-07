#[test]
fn background_parallelism_child() {
    if let Ok(expected) = std::env::var("HYSTERIAX_TEST_EXPECT_BACKGROUND_PARALLELISM") {
        assert_eq!(
            super::background_worker_parallelism(),
            expected.parse::<usize>().unwrap()
        );
    }
}

#[test]
fn background_parallelism_defaults_to_sixteen_and_accepts_bounded_overrides() {
    for (configured, expected) in [
        (None, 16),
        (Some("1"), 1),
        (Some("4"), 4),
        (Some("8"), 8),
        (Some("16"), 16),
        (Some("0"), 16),
        (Some("17"), 16),
        (Some("32"), 16),
        (Some("invalid"), 16),
        (Some(""), 16),
    ] {
        let mut child = std::process::Command::new(std::env::current_exe().unwrap());
        child
            .args([
                "--exact",
                "runtime_config::tests::background_parallelism_child",
            ])
            .env(
                "HYSTERIAX_TEST_EXPECT_BACKGROUND_PARALLELISM",
                expected.to_string(),
            )
            .env_remove("HYSTERIAX_BACKGROUND_WORKER_PARALLELISM");
        if let Some(value) = configured {
            child.env("HYSTERIAX_BACKGROUND_WORKER_PARALLELISM", value);
        }
        let output = child.output().unwrap();
        assert!(
            output.status.success(),
            "configuration {configured:?}: {}",
            String::from_utf8_lossy(&output.stdout)
        );
    }
}

#[test]
fn traffic_capture_setting_child() {
    if let Ok(expected) = std::env::var("HYSTERIAX_TEST_CAPTURE_EXPECTED") {
        assert_eq!(super::traffic_capture_enabled(), expected == "true");
    }
}

#[test]
fn traffic_capture_setting_defaults_on_and_can_pause_capture() {
    for (configured, expected) in [
        (None, true),
        (Some("true"), true),
        (Some("0"), false),
        (Some("false"), false),
        (Some("invalid"), false),
    ] {
        let mut child = std::process::Command::new(std::env::current_exe().unwrap());
        child
            .args([
                "--exact",
                "runtime_config::tests::traffic_capture_setting_child",
            ])
            .env("HYSTERIAX_TEST_CAPTURE_EXPECTED", expected.to_string())
            .env_remove("HYSTERIAX_TRAFFIC_CAPTURE_ENABLED");
        if let Some(value) = configured {
            child.env("HYSTERIAX_TRAFFIC_CAPTURE_ENABLED", value);
        }
        assert!(
            child.output().unwrap().status.success(),
            "configuration {configured:?}"
        );
    }
}

#[test]
fn upload_timeout_child() {
    if let Ok(expected) = std::env::var("HYSTERIAX_TEST_EXPECT_UPLOAD_TIMEOUT") {
        assert_eq!(
            super::ssh_upload_timeout_seconds(),
            expected.parse::<u64>().unwrap()
        );
        assert_eq!(super::operation_timeout_seconds(15), 60);
    }
}

#[test]
fn upload_timeout_defaults_to_three_minutes_and_is_independent_of_operation_budget() {
    for (configured, expected) in [
        (None, 180),
        (Some("1"), 1),
        (Some("180"), 180),
        (Some("3600"), 3600),
        (Some("0"), 180),
        (Some("3601"), 180),
        (Some("invalid"), 180),
    ] {
        let mut child = std::process::Command::new(std::env::current_exe().unwrap());
        child
            .args(["--exact", "runtime_config::tests::upload_timeout_child"])
            .env("HYSTERIAX_TEST_EXPECT_UPLOAD_TIMEOUT", expected.to_string())
            .env("HYSTERIAX_OPERATION_TIMEOUT_SECONDS", "60")
            .env_remove("HYSTERIAX_SSH_UPLOAD_TIMEOUT_SECONDS");
        if let Some(value) = configured {
            child.env("HYSTERIAX_SSH_UPLOAD_TIMEOUT_SECONDS", value);
        }
        assert!(
            child.output().unwrap().status.success(),
            "configuration {configured:?}"
        );
    }
}

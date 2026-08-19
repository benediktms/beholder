use std::{fs, process::Command};

#[test]
fn reindex_reports_a_stable_error_code_when_the_daemon_is_unavailable() {
    let state = std::env::temp_dir().join(format!(
        "beholder-cli-operation-error-{}",
        std::process::id()
    ));
    let _ = fs::remove_dir_all(&state);
    fs::create_dir_all(&state).unwrap();

    let output = Command::new(env!("CARGO_BIN_EXE_beholder"))
        .env("BEHOLDER_STATE_DIR", &state)
        .args(["reindex-workspace", "main"])
        .output()
        .unwrap();

    assert!(!output.status.success());
    assert!(
        String::from_utf8(output.stderr)
            .unwrap()
            .contains("[beholder.daemon.unavailable] Beholder daemon is unavailable")
    );
    fs::remove_dir_all(state).unwrap();
}

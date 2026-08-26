use beholder_indexing::{
    AnalysisInputKind, PLUGIN_API_VERSION, PluginDescriptor, PluginInputScope, PluginInputSelector,
    PluginPathMatcher,
};
use beholder_worker_client::PluginRegistry;
use std::{collections::BTreeSet, fs, path::Path, process::Command};

fn state(name: &str) -> std::path::PathBuf {
    let path = std::env::temp_dir().join(format!("beholder-cli-{name}-{}", std::process::id()));
    let _ = fs::remove_dir_all(&path);
    fs::create_dir_all(&path).unwrap();
    path
}

fn beholder(state: &Path) -> Command {
    let mut command = Command::new(env!("CARGO_BIN_EXE_beholder"));
    command.env("BEHOLDER_STATE_DIR", state);
    command
}

fn plugin_descriptor(id: &str) -> PluginDescriptor {
    PluginDescriptor {
        id: id.into(),
        api_version: PLUGIN_API_VERSION,
        inputs: vec![PluginInputSelector {
            scope: PluginInputScope::Target,
            matcher: PluginPathMatcher::Extension("rs".into()),
            kind: AnalysisInputKind::Source,
        }],
        semantic_entities: BTreeSet::new(),
        semantic_relations: BTreeSet::new(),
        produces_entities: BTreeSet::new(),
        produces_relations: BTreeSet::new(),
    }
}

#[test]
fn process_lists_installed_plugin_descriptor_ids() {
    let state = state("plugin-list");
    let executable = state.join("plugin");
    fs::write(&executable, b"plugin").unwrap();
    let plugin = PluginRegistry::open(state.join("daemon"))
        .unwrap()
        .install(&executable, plugin_descriptor("example.kafka"), false)
        .unwrap();

    let output = beholder(&state).args(["plugin", "list"]).output().unwrap();

    assert!(output.status.success());
    assert!(output.stderr.is_empty());
    assert_eq!(
        String::from_utf8(output.stdout).unwrap(),
        format!("example.kafka\t{}\n", plugin.digest)
    );
    fs::remove_dir_all(state).unwrap();
}

#[test]
fn process_reports_usage_and_argument_failures_on_stderr() {
    let state = state("arguments");
    let output = beholder(&state).output().unwrap();
    assert!(!output.status.success());
    assert!(output.stdout.is_empty());
    assert!(
        String::from_utf8(output.stderr)
            .unwrap()
            .contains("Usage: beholder")
    );

    let output = beholder(&state)
        .args([
            "inspect",
            "relations",
            "--database",
            "missing.db",
            "--relation",
            "calls",
        ])
        .output()
        .unwrap();
    assert!(!output.status.success());
    assert!(output.stdout.is_empty());
    assert!(
        String::from_utf8(output.stderr)
            .unwrap()
            .contains("--relation is only valid for observations")
    );
    fs::remove_dir_all(state).unwrap();
}

#[test]
fn process_indexes_and_inspects_a_real_database() {
    let state = state("index");
    let source = state.join("sample.rs");
    let database = state.join("beholder.db");
    fs::write(&source, "fn caller() { callee(); } fn callee() {}").unwrap();

    let output = beholder(&state)
        .args(["index-rust", source.to_str().unwrap(), "--database"])
        .arg(&database)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(output.stderr.is_empty());
    assert!(
        String::from_utf8(output.stdout)
            .unwrap()
            .starts_with("indexed ")
    );

    let output = beholder(&state)
        .args(["index-rust", source.to_str().unwrap(), "--database"])
        .arg(&database)
        .output()
        .unwrap();
    assert!(output.status.success());
    assert_eq!(
        String::from_utf8(output.stdout).unwrap(),
        "unchanged; kept current analysis revision\n"
    );
    assert!(output.stderr.is_empty());

    let output = beholder(&state)
        .args(["inspect", "revisions", "--database"])
        .arg(&database)
        .output()
        .unwrap();
    assert!(output.status.success());
    assert!(output.stderr.is_empty());
    let stdout = String::from_utf8(output.stdout).unwrap();
    assert!(stdout.contains("revision"));
    assert!(stdout.contains("main"));
    fs::remove_dir_all(state).unwrap();
}

#[cfg(unix)]
#[test]
fn closed_output_pipe_exits_cleanly() {
    let state = state("broken-pipe");
    let source = state.join("large.rs");
    let database = state.join("beholder.db");
    let mut rust = String::from("fn caller() {");
    for index in 0..4_000 {
        rust.push_str(&format!("function_{index}();"));
    }
    rust.push('}');
    fs::write(&source, rust).unwrap();
    let indexed = beholder(&state)
        .args(["index-rust", source.to_str().unwrap(), "--database"])
        .arg(&database)
        .output()
        .unwrap();
    assert!(indexed.status.success());

    let output = Command::new("bash")
        .args([
            "-o",
            "pipefail",
            "-c",
            "\"$1\" inspect observations --database \"$2\" | head -n 1",
            "beholder-broken-pipe",
            env!("CARGO_BIN_EXE_beholder"),
            database.to_str().unwrap(),
        ])
        .env("BEHOLDER_STATE_DIR", &state)
        .output()
        .unwrap();

    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(
        !String::from_utf8(output.stderr)
            .unwrap()
            .contains("Broken pipe")
    );
    fs::remove_dir_all(state).unwrap();
}

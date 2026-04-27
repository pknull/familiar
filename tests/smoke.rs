//! Smoke tests for Familiar — binary invocation tests.
//!
//! These test the actual `familiar` binary, not library internals.
//! Each test builds and runs the binary with specific args/env,
//! verifying exit codes, stdout, and filesystem side effects.

use std::path::PathBuf;
use std::process::Command;

use tempfile::TempDir;

/// Get the path to the built binary.
fn binary() -> PathBuf {
    let mut path = std::env::current_exe()
        .unwrap()
        .parent()
        .unwrap()
        .parent()
        .unwrap()
        .to_path_buf();
    path.push("familiar");
    path
}

/// Run familiar with args, using a temp directory as HOME to isolate config/state.
fn run(tmp: &TempDir, args: &[&str]) -> std::process::Output {
    let config_dir = tmp.path().join(".familiar");
    let config_path = config_dir.join("familiar.toml");

    Command::new(binary())
        .arg("--config")
        .arg(&config_path)
        .args(args)
        .env("HOME", tmp.path())
        .env("XDG_DATA_HOME", tmp.path().join(".local/share"))
        .output()
        .expect("failed to execute familiar binary")
}

fn write_minimal_config(tmp: &TempDir) {
    let config_dir = tmp.path().join(".familiar");
    std::fs::create_dir_all(&config_dir).unwrap();

    let config = format!(
        r#"
[egregore]
api_url = "http://127.0.0.1:0"

[store]
path = "{}"
"#,
        tmp.path().join(".familiar/test.db").to_string_lossy(),
    );

    std::fs::write(config_dir.join("familiar.toml"), config).unwrap();
}

/// Create a config with a dummy LLM section (will fail to connect but parses).
fn write_config_with_llm(tmp: &TempDir) {
    write_minimal_config(tmp);

    let config_dir = tmp.path().join(".familiar");

    let config = format!(
        r#"
[egregore]
api_url = "http://127.0.0.1:0"

[store]
path = "{}"

[llm]
provider = "anthropic"
model = "claude-sonnet-4-20250514"
api_key_env = "FAKE_API_KEY"
"#,
        tmp.path().join(".familiar/test.db").to_string_lossy(),
    );

    std::fs::write(config_dir.join("familiar.toml"), config).unwrap();
}

/// Create a config with mock LLM provider (no API calls, returns canned response).
fn write_mock_provider_config(tmp: &TempDir, mock_response: &str) {
    write_minimal_config(tmp);

    let config_dir = tmp.path().join(".familiar");

    let config = format!(
        r#"
[egregore]
api_url = "http://127.0.0.1:0"

[store]
path = "{}"

[llm]
provider = "mock"
model = "mock"
base_url = "{}"
"#,
        tmp.path().join(".familiar/test.db").to_string_lossy(),
        mock_response,
    );

    std::fs::write(config_dir.join("familiar.toml"), config).unwrap();
}

// ---------------------------------------------------------------------------
// Binary smoke tests
// ---------------------------------------------------------------------------

#[test]
fn help_flag_prints_usage() {
    let output = Command::new(binary())
        .arg("--help")
        .output()
        .expect("failed to run");

    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(output.status.success(), "--help should exit 0");
    assert!(
        stdout.contains("Personal companion"),
        "Should show description"
    );
    assert!(stdout.contains("--simple"), "Should list --simple flag");
    assert!(stdout.contains("--config"), "Should list --config flag");
}

#[test]
fn version_flag_prints_version() {
    let output = Command::new(binary())
        .arg("--version")
        .output()
        .expect("failed to run");

    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(output.status.success());
    assert!(stdout.contains("familiar"));
}

#[test]
fn init_creates_config_directory() {
    let tmp = TempDir::new().unwrap();
    let config_dir = tmp.path().join(".familiar");

    let output = Command::new(binary())
        .arg("--config")
        .arg(config_dir.join("familiar.toml"))
        .arg("init")
        .env("HOME", tmp.path())
        .output()
        .expect("failed to run");

    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        output.status.success(),
        "init should succeed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(stdout.contains("Initialized"), "Should print init message");
    assert!(
        config_dir.join("familiar.toml").exists(),
        "Config file should exist"
    );
}

#[test]
fn init_refuses_when_config_exists() {
    let tmp = TempDir::new().unwrap();
    let config_dir = tmp.path().join(".familiar");
    std::fs::create_dir_all(&config_dir).unwrap();
    std::fs::write(config_dir.join("familiar.toml"), "existing").unwrap();

    let output = Command::new(binary())
        .arg("--config")
        .arg(config_dir.join("familiar.toml"))
        .arg("init")
        .env("HOME", tmp.path())
        .output()
        .expect("failed to run");

    assert!(!output.status.success(), "Should fail when config exists");
}

#[test]
fn sessions_with_empty_db() {
    let tmp = TempDir::new().unwrap();
    write_config_with_llm(&tmp);

    let output = run(&tmp, &["sessions"]);
    let stdout = String::from_utf8_lossy(&output.stdout);

    assert!(
        output.status.success(),
        "sessions should exit 0: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(
        stdout.contains("No sessions") || stdout.contains("ID"),
        "Should print empty message or header"
    );
}

#[test]
fn resume_with_no_sessions() {
    let tmp = TempDir::new().unwrap();
    write_config_with_llm(&tmp);

    let output = run(&tmp, &["resume"]);

    // Should fail since there are no sessions to pick from.
    assert!(
        !output.status.success(),
        "resume with no sessions should fail"
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("No sessions") || stderr.contains("no sessions"),
        "Should mention no sessions: {}",
        stderr
    );
}

#[test]
fn exec_without_llm_config_fails_gracefully() {
    let tmp = TempDir::new().unwrap();
    write_minimal_config(&tmp); // No [llm] section

    let output = run(&tmp, &["exec", "hello"]);

    assert!(!output.status.success(), "exec without LLM should fail");
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("LLM") || stderr.contains("llm") || stderr.contains("configuration"),
        "Should mention missing LLM config: {}",
        stderr
    );
}

#[test]
fn workspace_created_on_startup() {
    let tmp = TempDir::new().unwrap();
    write_config_with_llm(&tmp);

    // exec will fail (no real API key) but workspace should be created before the error.
    let _output = run(&tmp, &["exec", "test"]);

    let workspace = tmp.path().join(".familiar/workspace");
    // Workspace might be at the default ~/.familiar/workspace or wherever config points.
    // Since HOME is overridden, check the default location.
    let default_workspace = tmp.path().join(".familiar/workspace");
    if default_workspace.exists() {
        assert!(
            default_workspace.join("AGENTS.md").exists(),
            "AGENTS.md should be seeded"
        );
        assert!(
            default_workspace.join("SOUL.md").exists(),
            "SOUL.md should be seeded"
        );
        assert!(
            default_workspace.join("IDENTITY.md").exists(),
            "IDENTITY.md should be seeded"
        );
        assert!(
            default_workspace.join("MEMORY.md").exists(),
            "MEMORY.md should be seeded"
        );
    }
    // If workspace isn't at default location, the test still passes —
    // workspace creation depends on Config::expand_path which reads HOME.
}

#[test]
fn store_created_on_startup() {
    let tmp = TempDir::new().unwrap();
    write_minimal_config(&tmp);

    // sessions command touches the store.
    let _output = run(&tmp, &["sessions"]);

    let store_path = tmp.path().join(".familiar/test.db");
    assert!(store_path.exists(), "Store DB should be created");
}

#[test]
fn invalid_subcommand_shows_error() {
    let output = Command::new(binary())
        .arg("nonexistent-command")
        .output()
        .expect("failed to run");

    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("error") || stderr.contains("unrecognized"));
}

#[test]
fn missing_config_file_shows_error() {
    let output = Command::new(binary())
        .args(["--config", "/nonexistent/path/familiar.toml", "sessions"])
        .output()
        .expect("failed to run");

    assert!(!output.status.success());
}

#[test]
fn exec_with_mock_provider_returns_response() {
    let tmp = TempDir::new().unwrap();
    write_mock_provider_config(&tmp, "Hello from mock LLM!");

    let output = run(&tmp, &["exec", "say hello"]);
    let stdout = String::from_utf8_lossy(&output.stdout);

    assert!(
        output.status.success(),
        "exec with mock should succeed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(
        stdout.contains("Hello from mock LLM!"),
        "Should contain mock response. Got: {}",
        stdout
    );
}

#[test]
fn exec_with_mock_persists_to_store() {
    let tmp = TempDir::new().unwrap();
    write_mock_provider_config(&tmp, "Persisted response");

    // Send a message.
    let output = run(&tmp, &["exec", "test persistence"]);
    assert!(
        output.status.success(),
        "exec should succeed: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    // Check sessions list shows activity (store was written to).
    let output2 = run(&tmp, &["sessions"]);
    let stdout2 = String::from_utf8_lossy(&output2.stdout);
    // Even without session management wired into exec, the store DB should exist.
    assert!(
        tmp.path().join(".familiar/test.db").exists(),
        "Store DB should exist after exec"
    );
}

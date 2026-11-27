use serde_json::{json, Value};
use std::process::{Command, Output};
use tempfile::tempdir;

fn run_cli(data_dir: &std::path::Path, args: &[&str]) -> Output {
    let output = Command::new(env!("CARGO_BIN_EXE_docket"))
        .args(args)
        .env("DOCKET_DATA_DIR", data_dir)
        .output()
        .expect("run isolated docket CLI process");
    assert!(
        output.status.success(),
        "CLI failed: stdout={} stderr={}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    output
}

fn output_json(output: &Output) -> Value {
    serde_json::from_slice(&output.stdout).expect("CLI emits JSON")
}

#[test]
fn cli_reopens_the_same_isolated_sqlite_ledger_and_honors_data_dir_override() {
    let directory = tempdir().expect("isolated fixture home");
    let created = output_json(&run_cli(
        directory.path(),
        &[
            "create-item",
            "--source",
            "synthetic",
            "--title",
            "restart persistence fixture",
            "--context",
            "synthetic context only",
            "--recommendation",
            "verify across process restart",
            "--action-target",
            "hermes-coding",
            "--action-operation",
            "inspect_fixture",
            "--action-params",
            r#"{"opaque":{"value":[1,true,null]}}"#,
        ],
    ));
    let id = created["id"].as_str().expect("created item ID");
    assert_eq!(id, "dkt-000001");
    assert_eq!(
        created["action_descriptor"]["params"],
        json!({"opaque":{"value":[1,true,null]}})
    );
    assert!(directory.path().join("docket.sqlite3").is_file());

    let reopened = output_json(&run_cli(directory.path(), &["get-item", id]));
    assert_eq!(reopened["id"], id);
    assert_eq!(reopened["status"], "pending");
    assert_eq!(
        reopened["action_descriptor"]["params"],
        json!({"opaque":{"value":[1,true,null]}})
    );
    assert_eq!(reopened["context"], "synthetic context only");
}

//! JSON 入口退出码负测：信封 ok=false（或信封不可解析）必须以非零码
//! 退出——fixture/CI 门禁消费退出码，失败静默 exit 0 会让门禁形同虚设。

use std::process::Command;

fn run(args: &[&str]) -> std::process::Output {
    Command::new(env!("CARGO_BIN_EXE_uvp-core"))
        .args(args)
        .output()
        .expect("uvp-core binary should be runnable")
}

#[test]
fn json_entry_failures_exit_nonzero() {
    for args in [
        vec!["parse-hook", "not json at all"],
        vec!["eval-compiled-hook", "not json at all"],
        vec!["compile", "{\"definition\": 1}"],
        vec!["replay", "{\"events\": 1}"],
    ] {
        let output = run(&args);
        assert!(
            !output.status.success(),
            "{args:?} must exit non-zero on a failed JSON entry"
        );
        let stdout = String::from_utf8_lossy(&output.stdout);
        assert!(stdout.contains("\"ok\":false"), "{args:?}: {stdout}");
    }
}

#[test]
fn input_side_errors_exit_nonzero_with_envelope() {
    let output = run(&["compile", "@/nonexistent/definitely-missing.json"]);
    assert!(
        !output.status.success(),
        "unreadable @file must exit non-zero"
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("\"ok\":false") && stdout.contains("failed to read"),
        "{stdout}"
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(!stderr.contains("panic"), "must not panic: {stderr}");

    let output = run(&["lint-hook", "--profile", "bogus", "buyer::main.a"]);
    assert!(
        !output.status.success(),
        "unknown profile must exit non-zero"
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("\"ok\":false") && stdout.contains("evm_strict|cloud_compat"),
        "{stdout}"
    );

    let output = run(&[
        "lint-hook",
        "--profile",
        "x\", \"hook\": \"y",
        "buyer::main.a",
    ]);
    assert!(
        !output.status.success(),
        "injected profile must exit non-zero"
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    let envelope: serde_json::Value =
        serde_json::from_str(stdout.trim()).expect("stdout stays a parseable envelope");
    assert_eq!(envelope["ok"], serde_json::Value::Bool(false));

    let output = run(&["lint-hook", "--profile", "cloud_compat", "buyer::main.a"]);
    assert!(output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("\"ok\":true"), "{stdout}");
}

#[test]
fn lint_command_exit_codes_follow_deny_policy() {
    let dirty_yaml = "lint_dirty_zhixu.yaml";
    std::fs::write(
        dirty_yaml,
        r#"apiVersion: uvp/v0
kind: Zhixu
metadata:
  name: lint_cli_exit
spec:
  platform:
    type: cloud
  nucleation:
    id: core
  stages:
    - name: main
      source: buyer
      sendSignals:
        - name: a
      receiveSignals:
        DUP: "buyer::main.a & main.a"
      executor:
        supplierType: organization
        supplierID: org-lint
"#,
    )
    .expect("write fixture yaml");

    let output = run(&["lint", dirty_yaml]);
    assert!(
        output.status.success(),
        "diagnostics alone must not fail: {output:?}"
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("UVP-L001"),
        "stdout should list the diagnostic: {stdout}"
    );

    let output = run(&["lint", "--deny", "warning", dirty_yaml]);
    assert!(!output.status.success(), "--deny warning must fail the run");

    let output = run(&["lint", "--deny", "error", dirty_yaml]);
    assert!(
        output.status.success(),
        "--deny error must not trip on warnings"
    );

    let output = run(&["lint", "--deny", "UVP-L001", dirty_yaml]);
    assert!(
        !output.status.success(),
        "--deny UVP-L001 must fail the run"
    );

    let output = run(&["lint", "--deny", "warnings", dirty_yaml]);
    assert!(
        !output.status.success(),
        "typo deny tokens must fail loudly"
    );

    let output = run(&["lint", "--format=json", dirty_yaml]);
    assert!(output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);
    let envelope: serde_json::Value = serde_json::from_str(stdout.trim()).expect("json output");
    assert_eq!(envelope["ok"], serde_json::Value::Bool(true));
    assert_eq!(
        envelope["value"]["diagnostics"][0]["code"],
        serde_json::Value::String("UVP-L001".to_string())
    );

    let output = run(&["lint-hook", "buyer::main.a & main.a"]);
    assert!(output.status.success());
    assert!(String::from_utf8_lossy(&output.stdout).contains("UVP-L001"));

    let output = run(&["lint-hook", "buyer::main.a | ~main.a"]);
    assert!(
        !output.status.success(),
        "semantic rejection must exit non-zero"
    );
    assert!(String::from_utf8_lossy(&output.stdout).contains("\"ok\":false"));

    std::fs::remove_file(dirty_yaml).ok();
}

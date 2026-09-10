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

/// 输入侧有界失败：@file 读不到、非法 --profile 都输出 ok:false 信封并以
/// 非零码退出（与 JSON 入口的信封退出码契约同口径），绝不 panic。
#[test]
fn input_side_errors_exit_nonzero_with_envelope() {
    // @file 读取失败：ok:false 信封 + 非零退出。
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

    // --profile 闭集预校验：非法值在拼装前响亮拒绝（与 --deny token 的
    // 闭集校验同口径）。
    let output = run(&["lint-hook", "--profile", "bogus", "buyer::flow.main.a"]);
    assert!(!output.status.success(), "unknown profile must exit non-zero");
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("\"ok\":false") && stdout.contains("evm_strict|cloud_compat"),
        "{stdout}"
    );

    // 注入引号的 profile：同样命中闭集拒绝面，stdout 恒为可解析信封——
    // 请求体经 serde 序列化拼装，不再有裸 format! 内插的转义缺口。
    let output = run(&[
        "lint-hook",
        "--profile",
        "x\", \"hook\": \"y",
        "buyer::flow.main.a",
    ]);
    assert!(!output.status.success(), "injected profile must exit non-zero");
    let stdout = String::from_utf8_lossy(&output.stdout);
    let envelope: serde_json::Value =
        serde_json::from_str(stdout.trim()).expect("stdout stays a parseable envelope");
    assert_eq!(envelope["ok"], serde_json::Value::Bool(false));

    // 合法非默认 profile 照常工作。
    let output = run(&[
        "lint-hook",
        "--profile",
        "cloud_compat",
        "buyer::flow.main.a",
    ]);
    assert!(output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("\"ok\":true"), "{stdout}");
}

#[test]
fn json_entry_successes_exit_zero() {
    let output = run(&[
        "parse-hook",
        "{\"hookName\": \"HOOK\", \"hook\": \"buyer::task.main.cmp\"}",
    ]);
    assert!(output.status.success());
    assert!(String::from_utf8_lossy(&output.stdout).contains("\"ok\":true"));

    let output = run(&["version"]);
    assert!(output.status.success());
}

/// lint 子命令的退出码契约：诊断本身不是失败，只有显式 --deny（或定义
/// 非法、文件不可读）才非零退出（PRD 109 §4.1/§20）。
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
  taskPatterns:
    - name: flow
      stages:
        - name: main
          source: buyer
          sendSignals: ["a"]
          receiveSignals:
            DUP: "buyer::flow.main.a & flow.main.a"
          executor:
            supplierType: organization
            supplierID: org-lint
"#,
    )
    .expect("write fixture yaml");

    // 无 --deny：warning 级诊断仍 exit 0。
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

    // --deny warning：命中 warning 级，非零退出。
    let output = run(&["lint", "--deny", "warning", dirty_yaml]);
    assert!(!output.status.success(), "--deny warning must fail the run");

    // --deny error：warning 不命中，exit 0。
    let output = run(&["lint", "--deny", "error", dirty_yaml]);
    assert!(
        output.status.success(),
        "--deny error must not trip on warnings"
    );

    // --deny 按具体 code：UVP-L001 命中。
    let output = run(&["lint", "--deny", "UVP-L001", dirty_yaml]);
    assert!(
        !output.status.success(),
        "--deny UVP-L001 must fail the run"
    );

    // 拼写的 deny token 直接失败。
    let output = run(&["lint", "--deny", "warnings", dirty_yaml]);
    assert!(
        !output.status.success(),
        "typo deny tokens must fail loudly"
    );

    // --format=json：输出信封 JSON。
    let output = run(&["lint", "--format=json", dirty_yaml]);
    assert!(output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);
    let envelope: serde_json::Value = serde_json::from_str(stdout.trim()).expect("json output");
    assert_eq!(envelope["ok"], serde_json::Value::Bool(true));
    assert_eq!(
        envelope["value"]["diagnostics"][0]["code"],
        serde_json::Value::String("UVP-L001".to_string())
    );

    // lint-hook：单 hook 入口，信封退出码语义与其他 JSON 入口一致。
    let output = run(&["lint-hook", "buyer::flow.main.a & flow.main.a"]);
    assert!(output.status.success());
    assert!(String::from_utf8_lossy(&output.stdout).contains("UVP-L001"));

    let output = run(&["lint-hook", "buyer::flow.main.a | ~flow.main.a"]);
    assert!(
        !output.status.success(),
        "semantic rejection must exit non-zero"
    );
    assert!(String::from_utf8_lossy(&output.stdout).contains("\"ok\":false"));

    std::fs::remove_file(dirty_yaml).ok();
}

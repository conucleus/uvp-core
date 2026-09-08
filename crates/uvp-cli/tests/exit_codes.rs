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

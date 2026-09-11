use clap::{Parser, Subcommand};
use std::fs;
use std::process::ExitCode;

#[derive(Parser)]
#[command(name = "uvp-core")]
#[command(about = "UVP semantic core CLI")]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    ParseHook {
        request: String,
    },
    EvalCompiledHook {
        request: String,
    },
    Compile {
        request: String,
    },
    Replay {
        request: String,
    },
    /// Lint 一个 hook 表达式（PRD 109）。合法表达的 diagnostics 在
    /// ok:true 的 value 里；lint 结论不是解析失败。
    LintHook {
        /// hook 表达式原文（`source::condition`），或 `@file` 路径。
        hook: String,
        /// `evm_strict`（默认）或 `cloud_compat`。
        #[arg(long, default_value = "evm_strict")]
        profile: String,
    },
    /// Lint 一份 Zhixu 定义：semantic validation + 单 Hook 规则 + 同 Stage
    /// 关系规则。lint 不改变 compile 语义；只有显式 --deny 才影响退出码。
    Lint {
        /// Zhixu 定义文件路径（YAML 或 JSON）。
        path: String,
        /// `text`（默认）或 `json`（信封 JSON，供 Store / IDE / CI 消费）。
        #[arg(long, default_value = "text")]
        format: String,
        /// 阻塞策略，可重复：`error` / `warning` / `info`（按严重级）或
        /// 具体 lint code（如 UVP-L002）。命中即以非零码退出。
        #[arg(long = "deny")]
        deny: Vec<String>,
    },
    Version,
}

fn main() -> ExitCode {
    let cli = Cli::parse();
    match cli.command {
        Command::Version => {
            println!("{}", uvp_hook_dsl::CORE_VERSION);
            ExitCode::SUCCESS
        }
        Command::Lint { path, format, deny } => run_lint(&path, &format, &deny),
        command => {
            // 有界失败：输入侧错误（@file 读不到、非法 --profile）输出
            // ok:false 信封并以非零码退出，与 JSON 入口的信封退出码契约
            // 同口径——panic 只服务编程错误，不服务调用方输入。
            let output = match run(command) {
                Ok(output) => output,
                Err(message) => failure_envelope(&message),
            };
            println!("{output}");
            // JSON 入口以信封 ok 字段裁决退出码：fixture/CI 门禁消费退出码，
            // 失败仍 exit 0 会让门禁静默放行（信封不可解析按失败处理）。
            if envelope_failed(&output) {
                ExitCode::FAILURE
            } else {
                ExitCode::SUCCESS
            }
        }
    }
}

/// 调用方输入错误的信封形态：与库入口的失败信封同构（ok:false +
/// diagnostics），message 经 serde 转义，不做裸字符串拼接。
fn failure_envelope(message: &str) -> String {
    serde_json::to_string(&serde_json::json!({
        "ok": false,
        "diagnostics": [{ "message": message }],
    }))
    .expect("failure envelope should serialize")
}

fn run(command: Command) -> Result<String, String> {
    match command {
        Command::ParseHook { request } => Ok(uvp_hook_dsl::parse_hook_json(&read_arg(&request)?)),
        Command::EvalCompiledHook { request } => {
            Ok(uvp_hook_dsl::eval_compiled_hook_json(&read_arg(&request)?))
        }
        Command::Compile { request } => Ok(uvp_compiler::compile_json(&read_arg(&request)?)),
        Command::Replay { request } => Ok(uvp_replay::replay_json(&read_arg(&request)?)),
        Command::LintHook { hook, profile } => {
            // profile 闭集预校验：裸 format! 内插既不转义（profile 携带引号
            // 会拼出不可解析 JSON），也把非法值的报错推迟成信封内的 serde
            // 报错——拼装前响亮拒绝，与 --deny token 的闭集校验同口径。
            if !matches!(profile.as_str(), "evm_strict" | "cloud_compat") {
                return Err(format!(
                    "unknown --profile value {profile:?}: expected evm_strict|cloud_compat"
                ));
            }
            // 请求体经 serde_json 序列化拼装：hook 原文与 profile 都按 JSON
            // 字符串转义，杜绝手写内插的转义缺口。
            let request = serde_json::to_string(&serde_json::json!({
                "profile": profile,
                "hookName": "LINT",
                "hook": read_arg(&hook)?,
            }))
            .map_err(|err| err.to_string())?;
            Ok(uvp_hook_dsl::lint_hook_json(&request))
        }
        Command::Lint { .. } | Command::Version => unreachable!("handled in main"),
    }
}

/// lint 子命令：诊断永远不等于失败——除非调用方显式 --deny（PRD §4.1：
/// lint 不隐式扩大或缩小协议接受集合，阻塞策略归调用方）。
fn run_lint(path: &str, format: &str, deny: &[String]) -> ExitCode {
    let definition = match read_definition(path) {
        Ok(definition) => definition,
        Err(err) => {
            eprintln!("failed to read {path}: {err}");
            return ExitCode::FAILURE;
        }
    };
    let request = serde_json::json!({ "definition": definition }).to_string();
    let envelope = uvp_compiler::lint::lint_zhixu_json(&request);
    let value: serde_json::Value = match serde_json::from_str(&envelope) {
        Ok(value) => value,
        Err(err) => {
            eprintln!("unparseable lint envelope: {err}: {envelope}");
            return ExitCode::FAILURE;
        }
    };
    if envelope_failed(&envelope) {
        if format == "json" {
            println!("{envelope}");
        } else {
            let messages: Vec<&str> = value
                .get("diagnostics")
                .and_then(|item| item.as_array())
                .map(|items| {
                    items
                        .iter()
                        .filter_map(|item| item.get("message").and_then(|m| m.as_str()))
                        .collect()
                })
                .unwrap_or_default();
            eprintln!("lint did not run: {}", messages.join("; "));
        }
        return ExitCode::FAILURE;
    }
    let report = &value["value"];
    if format == "json" {
        println!("{envelope}");
    } else {
        print_lint_text(report);
    }
    match denied(&value, deny) {
        Ok(true) => ExitCode::FAILURE,
        Ok(false) => ExitCode::SUCCESS,
        Err(err) => {
            eprintln!("{err}");
            ExitCode::FAILURE
        }
    }
}

fn read_definition(path: &str) -> Result<serde_json::Value, String> {
    let content = fs::read_to_string(path).map_err(|err| err.to_string())?;
    if let Ok(value) = serde_json::from_str::<serde_json::Value>(&content) {
        return Ok(value);
    }
    serde_yaml::from_str::<serde_json::Value>(&content).map_err(|err| err.to_string())
}

fn print_lint_text(report: &serde_json::Value) {
    let diagnostics = report
        .get("diagnostics")
        .and_then(|item| item.as_array())
        .cloned()
        .unwrap_or_default();
    if diagnostics.is_empty() {
        println!("lint: no diagnostics");
        return;
    }
    for diagnostic in &diagnostics {
        let severity = diagnostic
            .get("severity")
            .and_then(|s| s.as_str())
            .unwrap_or("");
        let code = diagnostic
            .get("code")
            .and_then(|c| c.as_str())
            .unwrap_or("");
        let hook = diagnostic
            .get("hookName")
            .and_then(|h| h.as_str())
            .unwrap_or("-");
        let message = diagnostic
            .get("message")
            .and_then(|m| m.as_str())
            .unwrap_or("");
        println!("{severity:<8} {code} [{hook}] {message}");
        if let Some(explanation) = diagnostic.get("explanation").and_then(|e| e.as_str()) {
            println!("         {explanation}");
        }
    }
    println!(
        "lint: {} diagnostic(s) (semantic version {})",
        diagnostics.len(),
        report
            .get("semanticVersion")
            .and_then(|v| v.as_str())
            .unwrap_or("-")
    );
}

/// deny 策略裁决：`error` / `warning` / `info` 按严重级精确匹配，其余按
/// lint code 精确匹配。无法识别的 token 直接失败——拼错的策略名静默
/// 放行会把 CI 门禁变成摆设。
fn denied(envelope: &serde_json::Value, deny: &[String]) -> Result<bool, String> {
    if deny.is_empty() {
        return Ok(false);
    }
    let mut policy = Vec::new();
    for token in deny {
        match token.as_str() {
            "error" => policy.push(DenyRule::Severity("error")),
            "warning" => policy.push(DenyRule::Severity("warning")),
            "info" => policy.push(DenyRule::Severity("info")),
            code if code.starts_with("UVP-L") => policy.push(DenyRule::Code(code.to_string())),
            other => {
                return Err(format!(
                    "unknown --deny value {other:?}: expected error|warning|info or a UVP-L### lint code"
                ))
            }
        }
    }
    let diagnostics = envelope
        .get("value")
        .and_then(|value| value.get("diagnostics"))
        .and_then(|item| item.as_array())
        .cloned()
        .unwrap_or_default();
    Ok(diagnostics.iter().any(|diagnostic| {
        let severity = diagnostic
            .get("severity")
            .and_then(|s| s.as_str())
            .unwrap_or("");
        let code = diagnostic
            .get("code")
            .and_then(|c| c.as_str())
            .unwrap_or("");
        policy.iter().any(|rule| match rule {
            DenyRule::Severity(want) => severity == *want,
            DenyRule::Code(want) => code == want,
        })
    }))
}

enum DenyRule {
    Severity(&'static str),
    Code(String),
}

fn envelope_failed(output: &str) -> bool {
    serde_json::from_str::<serde_json::Value>(output)
        .ok()
        .and_then(|value| value.get("ok").and_then(|ok| ok.as_bool()))
        .is_none_or(|ok| !ok)
}

/// `@file` 形态的读取失败是调用方输入错误：向上传播为 ok:false 信封 +
/// 非零退出（有界失败），不 panic。
fn read_arg(value: &str) -> Result<String, String> {
    if let Some(path) = value.strip_prefix('@') {
        return fs::read_to_string(path).map_err(|err| format!("failed to read {path}: {err}"));
    }
    Ok(value.to_string())
}

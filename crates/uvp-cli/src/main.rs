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
    ParseHook { request: String },
    EvalCompiledHook { request: String },
    Compile { request: String },
    Replay { request: String },
    Version,
}

fn main() -> ExitCode {
    let cli = Cli::parse();
    match cli.command {
        Command::Version => {
            println!("{}", uvp_hook_dsl::CORE_VERSION);
            ExitCode::SUCCESS
        }
        command => {
            let output = run(command);
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

fn run(command: Command) -> String {
    match command {
        Command::ParseHook { request } => uvp_hook_dsl::parse_hook_json(&read_arg(&request)),
        Command::EvalCompiledHook { request } => {
            uvp_hook_dsl::eval_compiled_hook_json(&read_arg(&request))
        }
        Command::Compile { request } => uvp_compiler::compile_json(&read_arg(&request)),
        Command::Replay { request } => uvp_replay::replay_json(&read_arg(&request)),
        Command::Version => unreachable!("version handled in main"),
    }
}

fn envelope_failed(output: &str) -> bool {
    serde_json::from_str::<serde_json::Value>(output)
        .ok()
        .and_then(|value| value.get("ok").and_then(|ok| ok.as_bool()))
        .is_none_or(|ok| !ok)
}

fn read_arg(value: &str) -> String {
    if let Some(path) = value.strip_prefix('@') {
        return fs::read_to_string(path)
            .unwrap_or_else(|err| panic!("failed to read {path}: {err}"));
    }
    value.to_string()
}

use clap::{Parser, Subcommand};
use std::fs;

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
    EvalHook { request: String },
    Compile { request: String },
    Replay { request: String },
    Version,
}

fn main() {
    let cli = Cli::parse();
    let output = match cli.command {
        Command::ParseHook { request } => uvp_hook_dsl::parse_hook_json(&read_arg(&request)),
        Command::EvalHook { request } => uvp_hook_dsl::eval_hook_json(&read_arg(&request)),
        Command::Compile { request } => uvp_compiler::compile_json(&read_arg(&request)),
        Command::Replay { request } => uvp_replay::replay_json(&read_arg(&request)),
        Command::Version => uvp_hook_dsl::CORE_VERSION.to_string(),
    };
    println!("{output}");
}

fn read_arg(value: &str) -> String {
    if let Some(path) = value.strip_prefix('@') {
        return fs::read_to_string(path).unwrap_or_else(|err| {
            panic!("failed to read {path}: {err}");
        });
    }
    value.to_string()
}

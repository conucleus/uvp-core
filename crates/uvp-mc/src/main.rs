use std::fs;
use std::process::ExitCode;

use clap::{Parser, Subcommand};
use uvp_mc::{manifest::Manifest, mc_check, mc_lint_json, McCheckRequest};

#[derive(Parser)]
#[command(name = "uvp-mc")]
#[command(about = "Zhixu definition model checker (exhaustive, evaluator-as-oracle)")]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    Check {
        #[arg(long)]
        definition: String,
        #[arg(long)]
        manifest: String,
        #[arg(long)]
        json: bool,
    },
    Lint {
        #[arg(long)]
        definition: String,
        #[arg(long)]
        manifest: String,
    },
}

fn main() -> ExitCode {
    let cli = Cli::parse();
    match cli.command {
        Command::Check {
            definition,
            manifest,
            json,
        } => run_check(&definition, &manifest, json),
        Command::Lint { definition, manifest } => run_lint(&definition, &manifest),
    }
}

fn read_manifest(path: &str) -> Result<Manifest, String> {
    let raw = fs::read_to_string(path)
        .map_err(|err| format!("read manifest {path}: {err}"))?;
    if path.ends_with(".toml") {
        toml::from_str(&raw).map_err(|err| format!("parse manifest {path} as TOML: {err}"))
    } else {
        serde_json::from_str(&raw).map_err(|err| format!("parse manifest {path} as JSON: {err}"))
    }
}

fn read_definition(path: &str) -> Result<serde_json::Value, String> {
    let raw = fs::read_to_string(path)
        .map_err(|err| format!("read definition {path}: {err}"))?;
    serde_json::from_str(&raw).map_err(|err| format!("parse definition {path} as JSON: {err}"))
}

fn run_check(definition_path: &str, manifest_path: &str, json: bool) -> ExitCode {
    let result = (|| {
        let manifest = read_manifest(manifest_path)?;
        let definition = read_definition(definition_path)?;
        mc_check(McCheckRequest { definition, manifest }).map_err(|err| err.to_string())
    })();
    match result {
        Ok(report) => {
            if json {
                let envelope = serde_json::json!({
                    "ok": report.passed,
                    "value": report,
                });
                println!(
                    "{}",
                    serde_json::to_string_pretty(&envelope).expect("report should serialize")
                );
            } else {
                println!("zhixu: {}", report.zhixu_name);
                let lint = serde_json::to_string(&report.lint).unwrap_or_default();
                println!("lint: {lint}");
                for outcome in &report.checks {
                    println!(
                        "{}\t{}\tstates={}",
                        if outcome.status == uvp_mc::Status::Pass {
                            "PASS"
                        } else {
                            "FAIL"
                        },
                        outcome.id,
                        outcome.states
                    );
                    if let Some(violation) = &outcome.violation {
                        println!(
                            "  violation: {}",
                            serde_json::to_string_pretty(violation).unwrap_or_default()
                        );
                    }
                }
                println!("overall: {}", if report.passed { "PASS" } else { "FAIL" });
            }
            if report.passed {
                ExitCode::SUCCESS
            } else {
                ExitCode::FAILURE
            }
        }
        Err(message) => {
            eprintln!("uvp-mc: {message}");
            ExitCode::FAILURE
        }
    }
}

fn run_lint(definition_path: &str, manifest_path: &str) -> ExitCode {
    let result = (|| {
        let manifest = read_manifest(manifest_path)?;
        let definition = read_definition(definition_path)?;
        serde_json::to_value(McLintPayload { definition, manifest })
            .map_err(|err| err.to_string())
    })();
    match result {
        Ok(payload) => {
            let raw = serde_json::to_string(&payload).expect("payload should serialize");
            let output = mc_lint_json(&raw);
            let value: serde_json::Value =
                serde_json::from_str(&output).expect("lint envelope should parse");
            println!("{output}");
            if value["ok"] == serde_json::json!(true) {
                ExitCode::SUCCESS
            } else {
                ExitCode::FAILURE
            }
        }
        Err(message) => {
            eprintln!("uvp-mc: {message}");
            ExitCode::FAILURE
        }
    }
}

#[derive(serde::Serialize)]
#[serde(rename_all = "snake_case")]
struct McLintPayload {
    definition: serde_json::Value,
    manifest: Manifest,
}

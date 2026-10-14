//! CLI 入口：裸跑（--definition 一个文件零前置出结果）、flag 叠加性质
//! （ad-hoc 人肉查询）、--request JSON（机器入口，backend mcguard 用）。

use std::fs;
use std::process::ExitCode;

use clap::{Parser, Subcommand};
use uvp_mc::checks::CheckKind;
use uvp_mc::{mc_check, mc_lint_json, Check, McCheckRequest};

#[derive(Parser)]
#[command(name = "uvp-mc")]
#[command(about = "Zhixu definition model checker (exhaustive, evaluator-as-oracle)")]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Bare run (self-consistency + intent coexistence) plus optional flag checks
    #[command(group = clap::ArgGroup::new("check_source").required(true).args(["definition", "request"]))]
    Check {
        #[arg(long)]
        definition: Option<String>,
        #[arg(long)]
        request: Option<String>,
        #[arg(long = "bad-state", value_name = "EXPR")]
        bad_states: Vec<String>,
        #[arg(long, value_name = "EXPR")]
        reach: Vec<String>,
        #[arg(long = "deadlock-when", value_name = "WHEN")]
        deadlock_whens: Vec<String>,
        #[arg(long = "closed-when", value_name = "WHEN", requires = "close_intents")]
        closed_when: Option<String>,
        #[arg(long = "close-intents", value_name = "A,B", requires = "closed_when")]
        close_intents: Option<String>,
        #[arg(long = "max-states", value_name = "N")]
        max_states: Option<u64>,
        #[arg(long)]
        json: bool,
    },
    Lint {
        #[arg(long)]
        definition: String,
    },
    LintHooks {
        #[arg(long)]
        definition: String,
        #[arg(long)]
        json: bool,
    },
}

fn main() -> ExitCode {
    let cli = Cli::parse();
    match cli.command {
        Command::Check {
            definition,
            request,
            bad_states,
            reach,
            deadlock_whens,
            closed_when,
            close_intents,
            max_states,
            json,
        } => run_check(
            definition, request, bad_states, reach, deadlock_whens, closed_when,
            close_intents, max_states, json,
        ),
        Command::Lint { definition } => run_lint(&definition),
        Command::LintHooks { definition, json } => run_lint_hooks(&definition, json),
    }
}

#[allow(clippy::too_many_arguments)]
fn run_check(
    definition_path: Option<String>,
    request_path: Option<String>,
    bad_states: Vec<String>,
    reach: Vec<String>,
    deadlock_whens: Vec<String>,
    closed_when: Option<String>,
    close_intents: Option<String>,
    max_states: Option<u64>,
    json: bool,
) -> ExitCode {
    let result = (|| {
        if let Some(request_path) = request_path {
            let raw = fs::read_to_string(&request_path)
                .map_err(|err| format!("read request {request_path}: {err}"))?;
            return serde_json::from_str::<McCheckRequest>(&raw)
                .map_err(|err| format!("parse request {request_path} as JSON: {err}"))
                .and_then(|request| mc_check(request).map_err(|err| err.to_string()));
        }
        let definition_path = definition_path.expect("clap enforces definition or request");
        let mut checks = Vec::new();
        for (index, predicate) in bad_states.iter().enumerate() {
            checks.push(Check {
                id: format!("bad-state:{}", index + 1),
                kind: CheckKind::BadState,
                description: None,
                when: None,
                predicate: Some(predicate.clone()),
                intents: Vec::new(),
            });
        }
        for (index, predicate) in reach.iter().enumerate() {
            checks.push(Check {
                id: format!("reach:{}", index + 1),
                kind: CheckKind::CoReach,
                description: None,
                when: None,
                predicate: Some(predicate.clone()),
                intents: Vec::new(),
            });
        }
        for (index, when) in deadlock_whens.iter().enumerate() {
            checks.push(Check {
                id: format!("deadlock-when:{}", index + 1),
                kind: CheckKind::DeadlockFree,
                description: None,
                when: Some(when.clone()),
                predicate: None,
                intents: Vec::new(),
            });
        }
        if let Some(when) = closed_when {
            let intents = close_intents
                .expect("clap pairs closed-when with close-intents")
                .split(',')
                .map(|item| item.trim().to_string())
                .filter(|item| !item.is_empty())
                .collect::<Vec<_>>();
            checks.push(Check {
                id: "admission-closed".to_string(),
                kind: CheckKind::AdmissionClosed,
                description: None,
                when: Some(when),
                predicate: None,
                intents,
            });
        }
        let definition = read_definition(&definition_path)?;
        mc_check(McCheckRequest {
            definition,
            checks,
            max_states,
        })
        .map_err(|err| err.to_string())
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
                if let Some(pairs) = &report.intent_pairs {
                    println!("intent pairs:");
                    for pair in pairs {
                        println!(
                            "  {}\t{}\t{}",
                            pair.a,
                            pair.b,
                            if pair.coexist { "coexist" } else { "mutex" }
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

fn read_definition(path: &str) -> Result<serde_json::Value, String> {
    let raw = fs::read_to_string(path).map_err(|err| format!("read definition {path}: {err}"))?;
    serde_json::from_str(&raw).map_err(|err| format!("parse definition {path} as JSON: {err}"))
}

fn run_lint(definition_path: &str) -> ExitCode {
    let result = (|| {
        let definition = read_definition(definition_path)?;
        let payload = serde_json::json!({"definition": definition, "checks": []});
        let output = mc_lint_json(&payload.to_string());
        let value: serde_json::Value =
            serde_json::from_str(&output).expect("lint envelope should parse");
        println!("{output}");
        if value["ok"] == serde_json::json!(true) {
            Ok(())
        } else {
            Err("lint reported findings".to_string())
        }
    })();
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(message) => {
            eprintln!("uvp-mc: {message}");
            ExitCode::FAILURE
        }
    }
}

fn run_lint_hooks(definition_path: &str, json: bool) -> ExitCode {
    let result = (|| {
        let definition = read_definition(definition_path)?;
        uvp_mc::lint_hooks(&definition).map_err(|err| err.to_string())
    })();
    match result {
        Ok(report) => {
            if json {
                println!(
                    "{}",
                    serde_json::to_string_pretty(&report).expect("report should serialize")
                );
            } else {
                println!("zhixu: {}", report.zhixu_name);
                println!(
                    "guards examined: {} (subscription skipped: {}), conjuncts: {}{}",
                    report.guards_examined,
                    report.subscription_guards_skipped,
                    report.conjuncts_examined,
                    if report.search_budget_exhausted {
                        " [search budget exhausted: unflagged items may hide findings]"
                    } else {
                        ""
                    }
                );
                if report.findings.is_empty() {
                    println!("removable conjuncts: none");
                }
                for finding in &report.findings {
                    println!("REMOVABLE\t{}\t{}", finding.hook, finding.conjunct);
                }
            }
            ExitCode::SUCCESS
        }
        Err(message) => {
            eprintln!("uvp-mc: {message}");
            ExitCode::FAILURE
        }
    }
}

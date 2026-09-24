//! UVP Core Lint v1 回归门（PRD 109 §23）：
//!
//! * 正例语料（fixtures/hook/lint.v1.json）覆盖 §23.1 全部形态；
//! * 负回归钉住 §23.2：嵌套延时不提示折叠、`A | ~A` 属 semantic
//!   validation 拒绝而非 lint tautology；
//! * span 映射钉住解析器侧表的后序配对不变量；
//! * 随机合法表达式永不 panic、lint 不改变 parse/compile 语义（§23.4 的
//!   无 cargo-fuzz 基础设施下的确定性随机门）。

use serde_json::Value;
use uvp_hook_dsl::{
    lint_hook, lint_hook_json, Expr, Gate, Profile, SpannedExpr, MAX_LINT_BOOLEAN_DEPTH,
    MAX_LINT_BOOLEAN_OPERANDS,
};

const CORPUS: &str = include_str!("../../../fixtures/hook/lint.v1.json");

#[derive(serde::Deserialize)]
#[serde(rename_all = "camelCase")]
struct LintCorpus {
    cases: Vec<LintCase>,
    #[serde(rename = "invalidCases")]
    invalid_cases: Vec<InvalidCase>,
}

#[derive(serde::Deserialize)]
#[serde(rename_all = "camelCase")]
struct LintCase {
    name: String,
    hook_name: String,
    hook: String,
    expect_codes: Vec<String>,
    #[serde(default)]
    forbidden_message_substrings: Vec<String>,
}

#[derive(serde::Deserialize)]
#[serde(rename_all = "camelCase")]
struct InvalidCase {
    name: String,
    hook_name: String,
    hook: String,
    message_contains: String,
}

fn load_corpus() -> LintCorpus {
    serde_json::from_str(CORPUS).expect("lint corpus fixture should parse")
}

fn codes(report: &uvp_hook_dsl::LintReport) -> Vec<String> {
    let mut codes: Vec<String> = report
        .diagnostics
        .iter()
        .map(|diagnostic| diagnostic.code.to_string())
        .collect();
    codes.sort();
    codes.dedup();
    codes
}

#[test]
fn lint_corpus_positive_and_negative_cases() {
    let corpus = load_corpus();
    assert!(!corpus.cases.is_empty());
    for case in &corpus.cases {
        let report = lint_hook(Profile::EvmStrict, &case.hook_name, &case.hook)
            .unwrap_or_else(|err| panic!("case {:?} must be a legal hook: {err}", case.name));
        let mut expected = case.expect_codes.clone();
        expected.sort();
        assert_eq!(
            codes(&report),
            expected,
            "case {:?} ({}) produced unexpected diagnostics: {:?}",
            case.name,
            case.hook,
            report
                .diagnostics
                .iter()
                .map(|diagnostic| format!("{} {}", diagnostic.code, diagnostic.message))
                .collect::<Vec<_>>()
        );
        for substring in &case.forbidden_message_substrings {
            for diagnostic in &report.diagnostics {
                let hay = format!(
                    "{} {}",
                    diagnostic.message,
                    diagnostic.explanation.clone().unwrap_or_default()
                );
                assert!(
                    !hay.contains(substring.as_str()),
                    "case {:?} must not suggest {substring:?}: {}",
                    case.name,
                    hay
                );
            }
        }
        assert_eq!(report.semantic_version, "uvp.semantic.v1");
    }
}

#[test]
fn lint_corpus_semantic_validation_rejections() {
    let corpus = load_corpus();
    for case in &corpus.invalid_cases {
        match lint_hook(Profile::EvmStrict, &case.hook_name, &case.hook) {
            Err(err) => assert!(
                err.to_string().contains(&case.message_contains),
                "case {:?}: unexpected error {err}",
                case.name
            ),
            Ok(report) => panic!(
                "case {:?} must be rejected by semantic validation, got {:?}",
                case.name,
                codes(&report)
            ),
        }
    }
}

/// lint 失败不改变 parse 语义：同一 hook 的 parse_hook 输出在 lint 前后
/// 逐字节一致（lint 是只读旁路，PRD §26 第 17/18 条）。
#[test]
fn lint_does_not_change_parse_semantics() {
    for hook in [
        "buyer::task.a.cmp & task.a.cmp",
        "buyer::(task.a.cmp +10s) & (task.a.cmp +5s)",
        "buyer::task.a.cmp & (task.a.cmp | task.b.cmp)",
    ] {
        let before = uvp_hook_dsl::parse_hook(uvp_hook_dsl::ParseHookRequest {
            profile: Profile::EvmStrict,
            gate: Gate::Hook,
            hook_name: "HOOK".to_string(),
            hook: hook.to_string(),
        })
        .expect("hook must parse");
        let lint_report = lint_hook(Profile::EvmStrict, "HOOK", hook).expect("hook must lint");
        let after = uvp_hook_dsl::parse_hook(uvp_hook_dsl::ParseHookRequest {
            profile: Profile::EvmStrict,
            gate: Gate::Hook,
            hook_name: "HOOK".to_string(),
            hook: hook.to_string(),
        })
        .expect("hook must parse again");
        assert_eq!(before, after, "lint must not mutate parse semantics");
        assert!(
            !lint_report.diagnostics.is_empty(),
            "fixture {hook} is expected to be lint-dirty"
        );
    }
}

/// span 侧表的后序配对不变量：diagnostic 的 span 必须切出条件原文的真实
/// 子串（primary operand 本身）。
#[test]
fn diagnostic_spans_point_at_real_source_fragments() {
    let report = lint_hook(
        Profile::EvmStrict,
        "DUP",
        "buyer::task.pay.cmp  &  task.pay.cmp",
    )
    .unwrap();
    let duplicate = report
        .diagnostics
        .iter()
        .find(|diagnostic| diagnostic.code == "UVP-L001")
        .expect("duplicate-term must fire");
    let condition = "task.pay.cmp  &  task.pay.cmp";
    let primary = duplicate.primary_span.expect("span must be present");
    assert_eq!(
        &condition[primary.start_byte..primary.end_byte],
        "task.pay.cmp"
    );
    assert_eq!(duplicate.related_spans.len(), 1);
    let related = duplicate.related_spans[0].span;
    assert_eq!(
        &condition[related.start_byte..related.end_byte],
        "task.pay.cmp"
    );
    assert!(related.start_byte > primary.end_byte);

    // 组节点 span 覆盖整个组；嵌套结构下每个 group 的 span 仍是原文切片。
    // 括号本身不进入组 span（span 记录在组自身 token 上，这是 v1 的已知
    // 外观边界，诊断仍能精确指向操作数）。
    let report = lint_hook(
        Profile::EvmStrict,
        "ABS",
        "buyer::task.pay.cmp & (task.pay.cmp | task.refund.cmp)",
    )
    .unwrap();
    let absorption = report
        .diagnostics
        .iter()
        .find(|diagnostic| diagnostic.code == "UVP-L003")
        .expect("absorption must fire");
    let condition = "task.pay.cmp & (task.pay.cmp | task.refund.cmp)";
    let primary = absorption.primary_span.expect("span must be present");
    assert_eq!(
        &condition[primary.start_byte..primary.end_byte],
        "task.pay.cmp | task.refund.cmp"
    );
}

/// L006：boolean nesting depth 超预算（预算 8，与 parser 深度限制无关）。
#[test]
fn excessive_boolean_depth_is_reported() {
    let mut condition = String::from("task.a.cmp");
    for _ in 0..(MAX_LINT_BOOLEAN_DEPTH + 1) {
        condition = format!("({condition} & task.b.cmp)");
    }
    let hook = format!("buyer::{condition}");
    let report = lint_hook(Profile::EvmStrict, "DEEP", &hook).unwrap();
    assert!(
        report
            .diagnostics
            .iter()
            .any(|diagnostic| diagnostic.code == "UVP-L006"),
        "depth {} must exceed the budget: {:?}",
        MAX_LINT_BOOLEAN_DEPTH + 1,
        codes(&report)
    );

    // 恰好在预算内不报告。
    let mut condition = String::from("task.a.cmp");
    for _ in 0..MAX_LINT_BOOLEAN_DEPTH {
        condition = format!("({condition} & task.b.cmp)");
    }
    let hook = format!("buyer::{condition}");
    let report = lint_hook(Profile::EvmStrict, "DEEP", &hook).unwrap();
    assert!(
        !report
            .diagnostics
            .iter()
            .any(|diagnostic| diagnostic.code == "UVP-L006"),
        "depth exactly at the budget must stay silent: {:?}",
        codes(&report)
    );
}

/// L007：单个 boolean group 的 operand 数量超预算。
#[test]
fn excessive_operand_count_is_reported() {
    let operands = (0..(MAX_LINT_BOOLEAN_OPERANDS + 1))
        .map(|index| format!("task.s{index}.cmp"))
        .collect::<Vec<_>>()
        .join(" & ");
    let hook = format!("buyer::{operands}");
    let report = lint_hook(Profile::EvmStrict, "WIDE", &hook).unwrap();
    assert!(
        report
            .diagnostics
            .iter()
            .any(|diagnostic| diagnostic.code == "UVP-L007"),
        "{}/{} operands must exceed the budget: {:?}",
        MAX_LINT_BOOLEAN_OPERANDS + 1,
        MAX_LINT_BOOLEAN_OPERANDS,
        codes(&report)
    );
}

/// lint JSON 入口的信封与序列化口径（camelCase 字段，供 CLI / FFI /
/// Store / IDE 消费）。
#[test]
fn lint_hook_json_envelope_shape() {
    let envelope = lint_hook_json(
        r#"{"profile": "evm_strict", "hookName": "DUP", "hook": "buyer::task.a.cmp & task.a.cmp"}"#,
    );
    let value: Value = serde_json::from_str(&envelope).unwrap();
    assert_eq!(value["ok"], Value::Bool(true));
    let diagnostic = &value["value"]["diagnostics"][0];
    assert_eq!(diagnostic["code"], Value::String("UVP-L001".to_string()));
    assert_eq!(diagnostic["severity"], Value::String("warning".to_string()));
    assert_eq!(
        diagnostic["category"],
        Value::String("redundancy".to_string())
    );
    assert_eq!(diagnostic["hookName"], Value::String("DUP".to_string()));
    assert!(diagnostic["primarySpan"]["startByte"].is_u64());
    assert_eq!(
        diagnostic["proof"]["kind"],
        Value::String("duplicate_term".to_string())
    );

    // 语义验证失败按 ok:false 返回，而不是 diagnostic。
    let envelope =
        lint_hook_json(r#"{"hookName": "TAUT", "hook": "buyer::task.a.cmp | ~task.a.cmp"}"#);
    let value: Value = serde_json::from_str(&envelope).unwrap();
    assert_eq!(value["ok"], Value::Bool(false));
    assert!(value["value"].is_null());
}

/// ready_implies 的关键 sound 性质（供 Layer 2 与规则内部共用）。
#[test]
fn ready_implies_soundness_spot_checks() {
    use uvp_hook_dsl::ready_implies;
    use uvp_hook_dsl::ProofResult::{Proven, Unknown};

    let signal = |name: &str| Expr::Signal(name.to_string());
    let a = signal("task.a.cmp");
    let b = signal("task.b.cmp");

    // And ⇒ 成员。
    assert!(matches!(
        ready_implies(&Expr::And(vec![a.clone(), b.clone()]), &a),
        Proven(_)
    ));
    // 成员不 ⇒ And（A 就绪不保证 B）。
    assert!(matches!(
        ready_implies(&a, &Expr::And(vec![a.clone(), b.clone()])),
        Unknown
    ));
    // Or 成员 ⇒ Or。
    assert!(matches!(
        ready_implies(&a, &Expr::Or(vec![a.clone(), b.clone()])),
        Proven(_)
    ));
    // Or 不 ⇒ 特定成员（不知道哪个分支获胜）。
    assert!(matches!(
        ready_implies(&Expr::Or(vec![a.clone(), b.clone()]), &a),
        Unknown
    ));
    // 延时支配：d1 ≥ d2 同基座。
    let delay = |expr: Expr, seconds: i64| Expr::Delay {
        expr: Box::new(expr),
        raw_duration: format!("{seconds}s"),
        duration_seconds: seconds,
    };
    assert!(matches!(
        ready_implies(&delay(a.clone(), 10), &delay(a.clone(), 5)),
        Proven(uvp_hook_dsl::ProofReason::DelayDominance)
    ));
    assert!(matches!(
        ready_implies(&delay(a.clone(), 5), &delay(a.clone(), 10)),
        Unknown
    ));
    // Delay ⇒ 内层（延时成熟 ⇒ 信号已存在）。
    assert!(matches!(
        ready_implies(&delay(a.clone(), 10), &a),
        Proven(_)
    ));
    // 内层不 ⇒ 延时（信号存在不等于延时已到期）。
    assert!(matches!(ready_implies(&a, &delay(a.clone(), 10)), Unknown));
    // 跨信号无公理（Layer 3 未开放）。
    assert!(matches!(ready_implies(&a, &b), Unknown));
}

/// SpannedExpr 与解析器侧表的数量/次序不变量：合法解析的 span 数恰等于
/// AST 节点数，build 后每个节点 span 都落在父节点 span 内。
#[test]
fn spanned_tree_matches_parser_side_table() {
    for hook in [
        "buyer::task.a.cmp",
        "buyer::task.a.cmp & ~task.b.cmp",
        "buyer::(task.a.cmp +10s) & (task.b.cmp | task.c.cmp)",
        "::ANCHOR(@seller::trade.listing.cmp)",
        "buyer::~task.a.cmp & (task.b.cmp & task.c.cmp)",
    ] {
        let (hook_expr, spans) =
            uvp_hook_dsl::parse_hook_expr_with_spans(hook).expect("hook must parse");
        let count = count_nodes(&hook_expr.condition);
        assert_eq!(
            spans.len(),
            count,
            "span side table must have one entry per AST node for {hook}"
        );
        let tree = SpannedExpr::build(&hook_expr.condition, &spans);
        assert_span_nesting(&tree, hook);
    }
}

fn count_nodes(expr: &Expr) -> usize {
    match expr {
        Expr::Signal(_) | Expr::Subscription { .. } => 1,
        Expr::Not(inner) => 1 + count_nodes(inner),
        Expr::And(terms) | Expr::Or(terms) => 1 + terms.iter().map(count_nodes).sum::<usize>(),
        Expr::Delay { expr: inner, .. } => 1 + count_nodes(inner),
    }
}

fn assert_span_nesting(node: &SpannedExpr, hook: &str) {
    for child in &node.children {
        assert!(
            child.span.start_byte >= node.span.start_byte
                && child.span.end_byte <= node.span.end_byte,
            "child span {:?} must nest inside parent span {:?} for {hook}",
            child.span,
            node.span
        );
        assert_span_nesting(child, hook);
    }
}

/// §23.4 随机门（确定性种子）：parser → semantic validation → lint 全链
/// 永不 panic；生成的合法 hook 要么 lint 干净要么带诊断，非法样本按
/// Err 返回，二者都不能 panic 或挂起。
#[test]
fn random_expressions_never_panic_in_lint() {
    let mut rng = Lcg::new(0x2026_0909);
    for iteration in 0..2000 {
        let hook = generate_random_hook(&mut rng);
        let result = std::panic::catch_unwind(|| {
            lint_hook(Profile::EvmStrict, &format!("R{iteration}"), &hook)
        });
        assert!(
            result.is_ok(),
            "lint must never panic on generated hook {hook:?} (iteration {iteration})"
        );
    }
}

struct Lcg(u64);

impl Lcg {
    fn new(seed: u64) -> Self {
        Lcg(seed
            .wrapping_mul(6364136223846793005)
            .wrapping_add(1442695040888963407))
    }
    fn next(&mut self) -> u64 {
        self.0 = self
            .0
            .wrapping_mul(6364136223846793005)
            .wrapping_add(1442695040888963407);
        self.0 >> 16
    }
    fn below(&mut self, bound: u64) -> u64 {
        self.next() % bound.max(1)
    }
}

/// 按文法构造随机 hook（深小组、多延时、重复项、负 guard、嵌套括号——
/// §23.4 的重点攻击面），并保证 OR 分支含正锚、Not 只包信号。
fn generate_random_hook(rng: &mut Lcg) -> String {
    let signal_count = 1 + rng.below(4) as usize;
    let signal = |index: usize| format!("task.s{index}.cmp");
    let mut operands = Vec::new();
    for index in 0..signal_count {
        let name = signal(index);
        operands.push(match rng.below(6) {
            0 => name,
            1 => name,
            2 => format!("~{name}"),
            3 => format!("({name} +{}s)", 1 + rng.below(30)),
            4 => format!("({name} +{}s)", 1 + rng.below(30)),
            _ => format!("({name} | task.t{index}.cmp)"),
        });
    }
    // 保证至少一个正锚：首操作数强制为正形态。
    operands[0] = match rng.below(2) {
        0 => signal(0),
        _ => format!("({} +{}s)", signal(0), 1 + rng.below(30)),
    };
    let joined = if rng.below(2) == 0 {
        operands.join(" & ")
    } else {
        operands
            .iter()
            .map(|operand| {
                // OR 分支不得是纯负 guard：包一层正锚。
                if operand.starts_with('~') {
                    format!("({operand} & {})", signal(0))
                } else {
                    operand.clone()
                }
            })
            .collect::<Vec<_>>()
            .join(" | ")
    };
    let source = ["buyer", "seller", "platform"][rng.below(3) as usize];
    format!("{source}::{joined}")
}

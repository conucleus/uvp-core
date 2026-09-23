//! AST 与规范化：表达式树（Expr/HookExpr）、身份闸、Tight/Cloud 双面
//! 归一化与 cloud AST 序列化。本模块不含解析流程与求值流程。

use serde::Serialize;
use serde_json::{json, Value};

use crate::{HookError, Profile, Result, CLOUD_AST_SCHEMA_VERSION, RETIRED_KEYWORDS_HINT};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Expr {
    Signal(String),
    /// 跨源订阅通道：`ANCHOR(@source::task.stage.signal)`。
    /// 按类寻址（source 为 zhixu 局部因果身份类），逐事件投递并携带溯源，
    /// 无表达式裁决。路由（按单 / 扇入）由接收方锚定状态决定，聚合判定
    /// 归订阅方执行器；per-fact 代铸由阶段级 `mint` 声明表达，不属于 hook。
    Subscription {
        source: String,
        target: String,
    },
    Not(Box<Expr>),
    And(Vec<Expr>),
    Or(Vec<Expr>),
    Delay {
        expr: Box<Expr>,
        raw_duration: String,
        duration_seconds: i64,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HookExpr {
    pub raw: String,
    pub source: String,
    pub condition: Expr,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SubscriptionTarget {
    pub source: String,
    pub signal_name: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Compatibility {
    Portable,
    CloudOnly,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum HookMode {
    Normal,
    Subscription,
}

// Subscription operators are cross-source delivery channels, not
// backend/executor input declarations. Backend/executor external inputs are
// sent to UVP only when the executor explicitly chooses to do so; there is no
// externalSignals declaration.
pub(crate) fn validate_subscription_position(expr: &Expr, root: bool) -> Result<()> {
    match expr {
        Expr::Subscription { .. } => {
            if !root {
                return Err(HookError::Message(format!(
                    "{RETIRED_KEYWORDS_HINT}; a subscription must be the complete hook condition"
                )));
            }
            Ok(())
        }
        Expr::Signal(_) => Ok(()),
        Expr::Not(inner) => validate_subscription_position(inner, false),
        Expr::And(terms) | Expr::Or(terms) => {
            for term in terms {
                validate_subscription_position(term, false)?;
            }
            Ok(())
        }
        Expr::Delay { expr, .. } => validate_subscription_position(expr, false),
    }
}

pub(crate) fn validate_hook(expr: &Expr) -> Result<()> {
    let anchored = validate_anchors(expr, false, false)?;
    if !anchored {
        return Err(HookError::Message(
            "hook condition must contain at least one positive signal anchor".to_string(),
        ));
    }
    if let Expr::Or(terms) = expr {
        for term in terms {
            if !has_positive_anchor(term) {
                return Err(HookError::Message(
                    "each OR branch must contain a positive signal anchor".to_string(),
                ));
            }
        }
    }
    Ok(())
}

/// `veto_slot`：当前节点是否是某个 And 的直接子项——这是衰减否决位
/// `~(A + duration)` 的合法位置。`inside_delay_operand`：当前子树是否
/// 位于某个 Delay 的操作数内——否决位的 Ready 会衰减，而 Delay 的成熟
/// 是永久的，二者组合会让外层延时锚定在已过期的否决上静默放行，因此
/// Delay 操作数内任何深度一律禁止。两闸合并：根位置、Or 子项、Not
/// 操作数、Delay 操作数内出现的否决位全部拒绝。
fn validate_anchors(expr: &Expr, veto_slot: bool, inside_delay_operand: bool) -> Result<bool> {
    match expr {
        Expr::Signal(_) | Expr::Subscription { .. } => Ok(true),
        Expr::Not(inner) => match inner.as_ref() {
            Expr::Signal(_) => Ok(false),
            Expr::Delay {
                expr: delay_operand,
                duration_seconds,
                ..
            } => {
                if !veto_slot || inside_delay_operand {
                    return Err(HookError::Message(
                        "decaying veto ~(signal+duration) is only allowed as a direct operand of a conjunction (e.g. B & ~(A+14d)); not at the root, under OR/NOT, or inside a delay operand"
                            .to_string(),
                    ));
                }
                // Delay 自身校验不因外层取反放松：正时长、操作数须有正锚。
                if *duration_seconds <= 0 {
                    return Err(HookError::Message("delay must be positive".to_string()));
                }
                let anchored = validate_anchors(delay_operand, false, true)?;
                if !anchored {
                    return Err(HookError::Message(
                        "delay requires a positive signal anchor".to_string(),
                    ));
                }
                Ok(false)
            }
            _ => Err(HookError::Message(
                "negation only supports direct signal references".to_string(),
            )),
        },
        Expr::Delay {
            expr,
            duration_seconds,
            ..
        } => {
            if *duration_seconds <= 0 {
                return Err(HookError::Message("delay must be positive".to_string()));
            }
            let anchored = validate_anchors(expr, false, true)?;
            if !anchored {
                return Err(HookError::Message(
                    "delay requires a positive signal anchor".to_string(),
                ));
            }
            Ok(true)
        }
        Expr::And(terms) => {
            let mut anchored = false;
            for term in terms {
                anchored |= validate_anchors(term, true, inside_delay_operand)?;
            }
            Ok(anchored)
        }
        Expr::Or(terms) => {
            let mut anchored = false;
            for term in terms {
                let term_anchored = validate_anchors(term, false, inside_delay_operand)?;
                if !term_anchored {
                    return Err(HookError::Message(
                        "each OR branch must contain a positive signal anchor".to_string(),
                    ));
                }
                anchored = true;
            }
            Ok(anchored)
        }
    }
}

fn has_positive_anchor(expr: &Expr) -> bool {
    match expr {
        Expr::Signal(_) | Expr::Subscription { .. } => true,
        Expr::Not(_) => false,
        Expr::Delay { expr, .. } => has_positive_anchor(expr),
        Expr::And(terms) | Expr::Or(terms) => terms.iter().any(has_positive_anchor),
    }
}

#[derive(Debug, Clone, Copy)]
pub(crate) enum NormalizeStyle {
    Tight,
    Cloud,
}

pub(crate) fn normalize_condition(expr: &Expr, style: NormalizeStyle) -> String {
    match style {
        NormalizeStyle::Tight => normalize_tight(expr),
        NormalizeStyle::Cloud => normalize_cloud(expr, 0),
    }
}

pub(crate) fn normalize_tight(expr: &Expr) -> String {
    match expr {
        Expr::Signal(signal) => signal.clone(),
        Expr::Subscription { source, target } => {
            format!("ANCHOR(@{source}::{target})")
        }
        Expr::Not(inner) => format!("~{}", normalize_for_unary_tight(inner)),
        Expr::Delay {
            expr, raw_duration, ..
        } => {
            format!("{}+{}", normalize_for_unary_tight(expr), raw_duration)
        }
        Expr::And(terms) => terms
            .iter()
            .map(normalize_for_join_tight)
            .collect::<Vec<_>>()
            .join("&"),
        Expr::Or(terms) => terms
            .iter()
            .map(normalize_for_join_tight)
            .collect::<Vec<_>>()
            .join("|"),
    }
}

fn normalize_for_unary_tight(expr: &Expr) -> String {
    match expr {
        Expr::Signal(_) | Expr::Subscription { .. } => normalize_tight(expr),
        _ => format!("({})", normalize_tight(expr)),
    }
}

fn normalize_for_join_tight(expr: &Expr) -> String {
    match expr {
        Expr::And(_) | Expr::Or(_) => format!("({})", normalize_tight(expr)),
        _ => normalize_tight(expr),
    }
}

fn normalize_cloud(expr: &Expr, parent_precedence: u8) -> String {
    let precedence = precedence(expr);
    let body = match expr {
        Expr::Signal(signal) => signal.clone(),
        Expr::Subscription { source, target } => {
            format!("ANCHOR(@{source}::{target})")
        }
        // 一元包裹（~ 与延时）的子表达式括号由递归调用的优先级闸统一
        // 产生（And/Or 低优先级、Delay 在 parent>0 时各自成组），这里不得
        // 再补一层——否则 Cloud 面产出 `~((A & B))` / `((A & B)) +5s` 的
        // 双重括号，与 Tight 面外观系统性分叉。
        Expr::Not(inner) => format!("~{}", normalize_cloud(inner, precedence)),
        Expr::Delay {
            expr, raw_duration, ..
        } => format!("{} + {raw_duration}", normalize_cloud(expr, precedence)),
        Expr::And(terms) => terms
            .iter()
            .map(|term| normalize_cloud(term, precedence))
            .collect::<Vec<_>>()
            .join(" & "),
        Expr::Or(terms) => terms
            .iter()
            .map(|term| normalize_cloud(term, precedence))
            .collect::<Vec<_>>()
            .join(" | "),
    };

    if precedence < parent_precedence && matches!(expr, Expr::And(_) | Expr::Or(_)) {
        return format!("({body})");
    }
    if matches!(expr, Expr::Delay { .. }) && parent_precedence > 0 {
        return format!("({body})");
    }
    body
}

fn precedence(expr: &Expr) -> u8 {
    match expr {
        Expr::Or(_) => 1,
        Expr::And(_) => 2,
        Expr::Not(_) | Expr::Delay { .. } => 3,
        Expr::Signal(_) | Expr::Subscription { .. } => 4,
    }
}

pub(crate) fn runtime_condition(
    hook: &HookExpr,
    _hook_name: &str,
    profile: Profile,
) -> Result<String> {
    if profile == Profile::EvmStrict {
        return Ok(normalize_condition(&hook.condition, NormalizeStyle::Tight));
    }
    match &hook.condition {
        Expr::Subscription { .. } => {
            Ok(normalize_condition(&hook.condition, NormalizeStyle::Tight))
        }
        _ => Ok(normalize_condition(&hook.condition, NormalizeStyle::Cloud)),
    }
}

pub(crate) fn hook_mode(expr: &Expr) -> HookMode {
    match expr {
        Expr::Subscription { .. } => HookMode::Subscription,
        _ => HookMode::Normal,
    }
}

pub(crate) fn compatibility_for(_hook: &HookExpr, profile: Profile) -> Compatibility {
    match profile {
        Profile::EvmStrict => Compatibility::Portable,
        Profile::CloudCompat => Compatibility::CloudOnly,
    }
}

pub(crate) fn hook_to_value(hook: &HookExpr) -> Value {
    json!({
        "raw": hook.raw,
        "source": hook.source,
        "condition": expr_to_ts_value(&hook.condition),
    })
}

fn expr_to_ts_value(expr: &Expr) -> Value {
    match expr {
        Expr::Signal(signal) => json!({ "kind": "signal", "signalName": signal }),
        Expr::Subscription { source, target } => json!({
            "kind": "subscription",
            "source": source,
            "signal": target
        }),
        Expr::Not(inner) => json!({ "kind": "not", "expr": expr_to_ts_value(inner) }),
        Expr::And(terms) => {
            json!({ "kind": "and", "terms": terms.iter().map(expr_to_ts_value).collect::<Vec<_>>() })
        }
        Expr::Or(terms) => {
            json!({ "kind": "or", "terms": terms.iter().map(expr_to_ts_value).collect::<Vec<_>>() })
        }
        Expr::Delay {
            expr,
            raw_duration,
            duration_seconds,
        } => json!({
            "kind": "delay",
            "expr": expr_to_ts_value(expr),
            "durationSeconds": duration_seconds,
            "rawDuration": raw_duration,
        }),
    }
}

pub(crate) fn cloud_ast_for(hook: &HookExpr, _hook_name: &str, _profile: Profile) -> Result<Value> {
    match &hook.condition {
        Expr::Subscription { source, target } => Ok(json!({
            "schemaVersion": CLOUD_AST_SCHEMA_VERSION,
            // 订阅钩子按事件逐次由状态机扇入或按对接记录路由投递；路由由接收
            // 方锚定状态裁决，聚合判定归订阅方执行器，per-fact 代铸归阶段
            // mint 声明。
            "source": "",
            "mode": HookMode::Subscription,
            "subscriptionTarget": {
                "source": source,
                "signal": target,
            },
            "root": expr_to_cloud_value(&hook.condition)
        })),
        _ => Ok(json!({
            "schemaVersion": CLOUD_AST_SCHEMA_VERSION,
            "source": hook.source.clone(),
            "mode": HookMode::Normal,
            "root": expr_to_cloud_value(&hook.condition)
        })),
    }
}

fn expr_to_cloud_value(expr: &Expr) -> Value {
    match expr {
        Expr::Signal(signal) => json!({ "type": "signal", "signal": signal }),
        Expr::Subscription { source, target } => json!({
            "type": "subscription",
            "source": source,
            "signal": target
        }),
        Expr::Not(inner) => json!({ "type": "neg", "expr": expr_to_cloud_value(inner) }),
        Expr::And(terms) => fold_cloud_terms("and", terms),
        Expr::Or(terms) => fold_cloud_terms("or", terms),
        Expr::Delay {
            expr,
            raw_duration,
            duration_seconds,
        } => json!({
            "type": "delay",
            "expr": expr_to_cloud_value(expr),
            "rawDuration": raw_duration,
            "durationSeconds": duration_seconds,
        }),
    }
}

fn fold_cloud_terms(kind: &str, terms: &[Expr]) -> Value {
    // 分治平衡折叠：扁平 n 项链若左斜折叠会得到深度 n-1 的 AST，请求 JSON
    // 经 serde_json 反序列化时有 128 层递归上限。平衡树深度 O(log n)，
    // 任意合法项数都远低于该限界（嵌套深度另由 MAX_PARSE_DEPTH=120 单一
    // 上闸约束，"能解析就能求值"）。
    fn fold_balanced(kind: &str, terms: &[Expr]) -> Value {
        match terms.len() {
            0 => Value::Null,
            1 => expr_to_cloud_value(&terms[0]),
            _ => {
                let mid = terms.len() / 2;
                let (left, right) = terms.split_at(mid);
                json!({
                    "type": kind,
                    "left": fold_balanced(kind, left),
                    "right": fold_balanced(kind, right),
                })
            }
        }
    }
    fold_balanced(kind, terms)
}

pub(crate) fn contains_nested_subscription(expr: &Expr) -> bool {
    match expr {
        Expr::Subscription { .. } => true,
        Expr::Signal(_) => false,
        Expr::Not(inner) | Expr::Delay { expr: inner, .. } => contains_nested_subscription(inner),
        Expr::And(terms) | Expr::Or(terms) => terms.iter().any(contains_nested_subscription),
    }
}

pub(crate) fn is_strict_signal_ref(value: &str) -> bool {
    let parts = value.split('.').collect::<Vec<_>>();
    parts.len() == 3 && parts.iter().all(|part| !part.is_empty())
}

/// 信号身份的单一闸：三段式 task.stage.signal、每段 plain identifier、
/// 全名 ≤100（individual_record.signal_name VARCHAR(100)）。解析期标识符
/// 扫描、事实键校验（signal_map）与 cloud AST 解码共用，保证三处口径
/// 收敛——任一入口放行的身份另两处必然接受。
pub(crate) fn valid_signal_identity(value: &str) -> bool {
    is_strict_signal_ref(value) && value.len() <= 100 && value.split('.').all(is_plain_identifier)
}

/// 普通标识符扫描规则：非空，且仅 ASCII 字母/数字/下划线/中划线。
/// 订阅 target 的 source 类与 signal 各段均按此规则扫描。
pub(crate) fn is_plain_identifier(value: &str) -> bool {
    !value.is_empty()
        && value
            .chars()
            .all(|ch| ch.is_ascii_alphanumeric() || ch == '_' || ch == '-')
}

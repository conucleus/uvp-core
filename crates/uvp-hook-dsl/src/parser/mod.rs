//! 解析：递归下降解析器、词法闸与 parse 管线（请求/产物类型 + 入口）。
//! 解析行为与 profile 无关（profile 只影响归一化/兼容性输出）。

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::ast::{
    cloud_ast_for, compatibility_for, hook_mode, hook_to_value, is_plain_identifier,
    is_strict_signal_ref, normalize_condition, runtime_condition, validate_hook,
    validate_subscription_position, Compatibility, Expr, HookExpr, HookMode, NormalizeStyle,
    SubscriptionTarget,
};
use crate::dependency::{extract_dependencies, Dependency};
use crate::lint::Span;
use crate::{HookError, Profile, Result, CORE_VERSION, RETIRED_KEYWORDS_HINT, SEMANTIC_VERSION};

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ParseHookOutput {
    pub uvp_core_version: &'static str,
    pub semantic_version: &'static str,
    pub profile: Profile,
    pub compatibility: Compatibility,
    pub hook_name: String,
    pub source: String,
    pub mode: HookMode,
    pub raw_hook: String,
    pub raw_condition: String,
    pub runtime_condition: String,
    pub normalized_expression: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub subscription_target: Option<SubscriptionTarget>,
    pub dependencies: Vec<Dependency>,
    pub ast: Value,
    pub cloud_ast: Value,
}

#[derive(Debug, Deserialize)]
// FFI/NAPI 最外层请求信封：未知字段确定性拒绝（拼错的调用方输入不得
// 被静默忽略成零值语义）。
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct ParseHookRequest {
    #[serde(default)]
    pub profile: Profile,
    #[serde(default)]
    pub hook_name: String,
    pub hook: String,
}

pub fn parse_hook(req: ParseHookRequest) -> Result<ParseHookOutput> {
    let profile = req.profile;
    let hook_name = req.hook_name;
    validate_hook_name(&hook_name)?;
    // 解析行为与 profile 无关（profile 只影响归一化/兼容性输出），
    // 因此 parse_hook_expr 不接收 profile。
    let (hook, _spans) = parse_hook_expr_with_spans(&req.hook)?;
    validate_hook(&hook.condition)?;

    let raw_condition = req
        .hook
        .split_once("::")
        .map(|(_, cond)| cond.trim().to_string())
        .unwrap_or_default();
    let mode = hook_mode(&hook.condition);
    let compatibility = compatibility_for(&hook, profile);
    let runtime_condition = runtime_condition(&hook, &hook_name, profile)?;
    let normalized_expression = format!(
        "{}::{}",
        hook.source,
        normalize_condition(&hook.condition, NormalizeStyle::Tight)
    );
    let dependencies = extract_dependencies(&hook, profile);
    let cloud_ast = cloud_ast_for(&hook, &hook_name, profile)?;
    let subscription_target = match &hook.condition {
        Expr::Subscription { source, target } => Some(SubscriptionTarget {
            source: source.clone(),
            signal_name: target.clone(),
        }),
        _ => None,
    };

    Ok(ParseHookOutput {
        uvp_core_version: CORE_VERSION,
        semantic_version: SEMANTIC_VERSION,
        profile,
        compatibility,
        hook_name,
        source: hook.source.clone(),
        mode,
        raw_hook: req.hook,
        raw_condition,
        runtime_condition,
        normalized_expression,
        subscription_target,
        dependencies,
        ast: hook_to_value(&hook),
        cloud_ast,
    })
}

/// 深度上限单一闸：解析器（递归下降）与求值器（cloud AST 解码）共用同一
/// 常量，且必须低于 serde_json 对请求 JSON 的 128 层递归反序列化上限——
/// 合法表达式编译出的 cloud AST 连同求值信封序列化后深度约 ≤ 该值 + 常数，
/// 保证"能解析就能求值"，不会在求值入口被 serde_json 以另一口径拒绝。
pub(crate) const MAX_PARSE_DEPTH: usize = 120;

/// hook 通道名闸（parse 与 lint 共用同一口径）：长度对齐 DDL 列宽
/// （hook_name VARCHAR(36)），'.' / '#' 分别是 canonical 信号名与 hookId
/// 的命名空间分隔符，携带即拒绝；空白字符（含首尾空格）同样拒绝——
/// 通道名进 hookId（stage#hook_name），两侧必须逐字节一致，含空白的
/// 名字是全仓响亮拒绝纪律下的确定性非法输入，不做 trim 归一。
pub(crate) fn validate_hook_name(hook_name: &str) -> Result<()> {
    if hook_name.trim().is_empty() || hook_name.len() > 36 {
        return Err(HookError::Message(
            "hook_name must be 1-36 characters".to_string(),
        ));
    }
    if hook_name.contains('.') || hook_name.contains('#') {
        return Err(HookError::Message(
            "hook_name must not contain '.' or '#'".to_string(),
        ));
    }
    if hook_name.chars().any(char::is_whitespace) {
        return Err(HookError::Message(
            "hook_name must not contain whitespace".to_string(),
        ));
    }
    Ok(())
}

/// 解析 hook 原文并返回条件 AST 与 span 侧表（lint / diagnostics 工具的
/// 低层入口）。侧表按"节点创建次序"推入——递归下降先完成全部操作数再
/// 包装父节点，该次序恰等于最终 AST 的后序遍历次序，lint 侧
/// `SpannedExpr::build` 按同一后序配对还原。span 只服务于 diagnostics，
/// 不改变 runtime AST 的形态与语义，也不进入 Cloud protocol artifact。
pub fn parse_hook_expr_with_spans(raw: &str) -> Result<(HookExpr, Vec<Span>)> {
    let (source, condition_raw) = raw
        .trim()
        .split_once("::")
        .ok_or_else(|| HookError::Message("hook expression must contain \"::\"".to_string()))?;
    let source = source.trim().to_string();
    let condition_raw = condition_raw.trim();
    if condition_raw.is_empty() {
        return Err(HookError::Message(
            "hook condition cannot be empty".to_string(),
        ));
    }
    if source.is_empty() && !starts_cross_source(condition_raw) {
        return Err(HookError::Message(
            "empty source is only allowed for ANCHOR(@…) subscription hooks".to_string(),
        ));
    }
    if !source.is_empty() {
        // 标头 source 类是路由键：解析期钉死长度与字符集（编译期 ≤36 上限
        // 严于落库列宽 source_zhixu_id VARCHAR(64)，对齐 Go 镜像
        // zhixu_schema.go 的 ≤36 与 plain-identifier 规则）。订阅形态
        // （::ANCHOR(@…)）标头恒为空，不受此限——订阅目标
        // source 在解析 ANCHOR 目标时按同值（≤36 + plain identifier）校验。
        if source.len() > 36 {
            return Err(HookError::Message(format!(
                "hook source class exceeds the maximum length of 36 characters: {source}"
            )));
        }
        if !is_plain_identifier(&source) {
            return Err(HookError::Message(format!(
                "hook source must be a plain identifier: {source}"
            )));
        }
    }
    reject_unsupported_operators(condition_raw)?;
    let mut parser = Parser::new(condition_raw);
    let condition = parser.parse()?;
    let spans = parser.spans;
    validate_subscription_position(&condition, true)?;
    if matches!(condition, Expr::Subscription { .. }) && !source.is_empty() {
        return Err(HookError::Message(
            "subscription entries must use an empty source header: ::ANCHOR(@source::task.stage.signal)"
                .to_string(),
        ));
    }
    Ok((
        HookExpr {
            raw: raw.to_string(),
            source,
            condition,
        },
        spans,
    ))
}

fn starts_cross_source(value: &str) -> bool {
    // 不受支持的关键字仍放行进解析器，以便命中精确的 unsupported 报错
    // 而非笼统的空标头报错。匹配必须落到完整 token 边界：关键字后随
    // 标识符字符（如 ::ANCHORX 伪前缀）不是关键字形态，
    // 不得绕过空标头门禁。扇入类标头不在词表内：
    // 其字面按通用语法错误（空标头门禁）拒绝，没有退役清单条目。
    ["ANCHOR", "OUTSIDE", "OUTSOURCE"].iter().any(|keyword| {
        let Some(rest) = value.strip_prefix(keyword) else {
            return false;
        };
        rest.chars()
            .next()
            .is_none_or(|ch| !(ch.is_ascii_alphanumeric() || matches!(ch, '_' | '.' | '-')))
    })
}

fn reject_unsupported_operators(condition: &str) -> Result<()> {
    if condition.contains("&&") {
        return Err(HookError::Message(format!(
            "unsupported operator && in {condition:?}"
        )));
    }
    if condition.contains("%%") {
        return Err(HookError::Message(format!(
            "unsupported operator %% in {condition:?}"
        )));
    }
    Ok(())
}

struct Parser<'a> {
    input: &'a str,
    index: usize,
    depth: usize,
    /// 节点 span 侧表：每个 AST 节点在创建时推入自己的源码区间，
    /// 推入次序 == 最终树的后序遍历次序（见 parse_hook_expr_with_spans）。
    spans: Vec<Span>,
}

/// 递归下降深度上限。hook 表达式来自外部可填写的模板定义，无界嵌套
/// （深层括号或连续 `~`）会打满调用栈直接 abort 宿主进程——栈溢出不可被
/// catch_unwind 捕获，必须在解析期以普通错误拒绝。
impl<'a> Parser<'a> {
    fn new(input: &'a str) -> Self {
        Self {
            input,
            index: 0,
            depth: 0,
            spans: Vec::new(),
        }
    }

    /// 当前 token 末尾（剥掉尾部空白）：group / 一元 / 延时节点的 span
    /// 终点。失败的前瞻 consume 会吞掉尾随空白，直接取 index 会让 span
    /// 无谓地覆盖行尾空白。
    fn span_end_here(&self) -> usize {
        let mut end = self.index.min(self.input.len());
        while end > 0 {
            match self.input[..end].chars().next_back() {
                Some(ch) if ch.is_whitespace() => end -= ch.len_utf8(),
                _ => break,
            }
        }
        end
    }

    fn guard_depth<T>(&mut self, parse: impl FnOnce(&mut Self) -> Result<T>) -> Result<T> {
        self.depth += 1;
        if self.depth > MAX_PARSE_DEPTH {
            self.depth -= 1;
            return Err(HookError::Message(format!(
                "hook expression nesting exceeds the maximum depth of {MAX_PARSE_DEPTH}"
            )));
        }
        let result = parse(self);
        self.depth -= 1;
        result
    }

    fn parse(&mut self) -> Result<Expr> {
        let expr = self.guard_depth(|parser| parser.parse_or())?;
        self.skip_ws();
        if !self.at_end() {
            return Err(HookError::Message(format!(
                "unexpected token at {}: {}",
                self.index,
                &self.input[self.index..]
            )));
        }
        Ok(expr)
    }

    fn parse_or(&mut self) -> Result<Expr> {
        self.guard_depth(|parser| parser.parse_or_inner())
    }

    fn parse_or_inner(&mut self) -> Result<Expr> {
        self.skip_ws();
        let start = self.index;
        let mut terms = vec![self.parse_and()?];
        while self.consume("|") {
            terms.push(self.parse_and()?);
        }
        Ok(if terms.len() == 1 {
            terms.remove(0)
        } else {
            self.spans.push(Span::new(start, self.span_end_here()));
            Expr::Or(terms)
        })
    }

    fn parse_and(&mut self) -> Result<Expr> {
        self.skip_ws();
        let start = self.index;
        let mut terms = vec![self.guard_depth(|parser| parser.parse_unary())?];
        while self.consume("&") {
            terms.push(self.guard_depth(|parser| parser.parse_unary())?);
        }
        Ok(if terms.len() == 1 {
            terms.remove(0)
        } else {
            self.spans.push(Span::new(start, self.span_end_here()));
            Expr::And(terms)
        })
    }

    fn parse_unary(&mut self) -> Result<Expr> {
        self.skip_ws();
        let start = self.index;
        if self.consume("~") {
            let inner = self.guard_depth(|parser| parser.parse_unary())?;
            self.spans.push(Span::new(start, self.span_end_here()));
            return Ok(Expr::Not(Box::new(inner)));
        }
        self.parse_postfix()
    }

    fn parse_postfix(&mut self) -> Result<Expr> {
        self.skip_ws();
        let start = self.index;
        let mut expr = self.parse_primary()?;
        self.skip_ws();
        if self.consume("+") {
            let raw_duration = self.read_duration()?;
            let duration_seconds = duration_to_seconds(&raw_duration)?;
            self.spans.push(Span::new(start, self.span_end_here()));
            expr = Expr::Delay {
                expr: Box::new(expr),
                raw_duration,
                duration_seconds,
            };
        }
        Ok(expr)
    }

    fn parse_primary(&mut self) -> Result<Expr> {
        self.skip_ws();
        if self.consume("(") {
            let expr = self.parse_or()?;
            if !self.consume(")") {
                return Err(HookError::Message(format!(
                    "expected ')' at {}",
                    self.index
                )));
            }
            return Ok(expr);
        }

        let ident_start = self.index;
        let ident = self.read_identifier()?;
        match ident.as_str() {
            "ANCHOR" => self.parse_subscription(ident_start),
            "OUTSIDE" | "OUTSOURCE" => Err(HookError::Message(format!(
                "{ident}@ is not supported: {RETIRED_KEYWORDS_HINT}"
            ))),
            _ => {
                if !is_strict_signal_ref(&ident) {
                    return Err(HookError::Message(format!(
                        "signal reference must use task.stage.signal: {ident}"
                    )));
                }
                self.spans
                    .push(Span::new(ident_start, self.span_end_here()));
                Ok(Expr::Signal(ident))
            }
        }
    }

    /// 订阅通道：`ANCHOR(@source::task.stage.signal)`。`ANCHOR@`（无括号
    /// 裸标头）写法不受支持；目标必须携带 @ 前缀的 source 类名空间。
    fn parse_subscription(&mut self, anchor_start: usize) -> Result<Expr> {
        self.skip_ws();
        if self.peek() == '@' {
            return Err(HookError::Message(format!(
                "ANCHOR@ header form is not supported: {RETIRED_KEYWORDS_HINT}"
            )));
        }
        if !self.consume("(") {
            return Err(HookError::Message(format!(
                "expected '(' after ANCHOR at {}",
                self.index
            )));
        }
        let target_raw = self.read_balanced_target()?;
        let target = target_raw.strip_prefix('@').ok_or_else(|| {
            HookError::Message(format!(
                "subscription target must be @source::task.stage.signal: {target_raw:?}"
            ))
        })?;
        let (source, signal) = target.split_once("::").ok_or_else(|| {
            HookError::Message(format!(
                "subscription target must be @source::task.stage.signal: {target_raw:?}"
            ))
        })?;
        // source 类命名空间复用普通标识符扫描规则：字符集 [A-Za-z0-9_-]，
        // 拒绝空格、括号、额外 :: 分隔与非 ASCII 字符。这里故意不 trim：
        // `ANCHOR` 的目标是一个严格 token，内部空格不能被规范化后放行，
        // 否则不同运行时可能对同一份原文产生不同的 signal key。
        // 长度与标头 source 同值：≤36 是编译期钉死的上限，严于落库列
        // hook_dependency.source_zhixu_id VARCHAR(64)——超长在解析期拒绝
        // 而不是拖到落库报 value too long。
        if !is_plain_identifier(source) {
            return Err(HookError::Message(format!(
                "subscription source must be a plain identifier: {source:?}"
            )));
        }
        if source.len() > 36 {
            return Err(HookError::Message(format!(
                "subscription source exceeds the maximum length of 36 characters: {source:?}"
            )));
        }
        // signal 全名落 signal_name 列（VARCHAR(100)），与普通标识符扫描同限。
        if signal.len() > 100 {
            return Err(HookError::Message(format!(
                "subscription target signal exceeds the maximum length of 100 characters: {signal:?}"
            )));
        }
        let segments = signal.split('.').collect::<Vec<_>>();
        if segments.len() != 3 || !segments.iter().all(|part| is_plain_identifier(part)) {
            return Err(HookError::Message(format!(
                "subscription target must use task.stage.signal: {signal:?}"
            )));
        }
        self.spans
            .push(Span::new(anchor_start, self.span_end_here()));
        Ok(Expr::Subscription {
            source: source.to_string(),
            target: signal.to_string(),
        })
    }

    fn read_balanced_target(&mut self) -> Result<String> {
        let mut depth = 1;
        let start = self.index;
        while !self.at_end() {
            let ch = self.peek();
            self.index += ch.len_utf8();
            if ch == '(' {
                depth += 1;
            } else if ch == ')' {
                depth -= 1;
                if depth == 0 {
                    return Ok(self.input[start..self.index - 1].to_string());
                }
            }
        }
        Err(HookError::Message("unterminated @() target".to_string()))
    }

    fn read_duration(&mut self) -> Result<String> {
        self.skip_ws();
        let start = self.index;
        while !self.at_end() {
            let ch = self.peek();
            if ch.is_ascii_digit() || matches!(ch, 's' | 'm' | 'h' | 'd') {
                self.index += ch.len_utf8();
            } else {
                break;
            }
        }
        let duration = self.input[start..self.index].to_string();
        if duration.is_empty() {
            return Err(HookError::Message("invalid duration: <empty>".to_string()));
        }
        duration_to_seconds(&duration)?;
        Ok(duration)
    }

    fn read_identifier(&mut self) -> Result<String> {
        self.skip_ws();
        let start = self.index;
        while !self.at_end() {
            let ch = self.peek();
            if ch.is_ascii_alphanumeric() || matches!(ch, '_' | '.' | '-') {
                self.index += ch.len_utf8();
            } else {
                break;
            }
        }
        if start == self.index {
            return Err(HookError::Message(format!(
                "expected identifier at {}",
                self.index
            )));
        }
        let ident = &self.input[start..self.index];
        // 标识符整体落 signal_name 列（task.stage.signal 全名，VARCHAR(100)）。
        if ident.len() > 100 {
            return Err(HookError::Message(format!(
                "identifier exceeds the maximum length of 100 characters: {}…",
                &ident[..32]
            )));
        }
        Ok(ident.to_string())
    }

    fn consume(&mut self, value: &str) -> bool {
        self.skip_ws();
        if self.input[self.index..].starts_with(value) {
            self.index += value.len();
            true
        } else {
            false
        }
    }

    fn skip_ws(&mut self) {
        while !self.at_end() {
            let ch = self.peek();
            if ch.is_whitespace() {
                self.index += ch.len_utf8();
            } else {
                break;
            }
        }
    }

    fn peek(&self) -> char {
        self.input[self.index..].chars().next().unwrap_or('\0')
    }

    fn at_end(&self) -> bool {
        self.index >= self.input.len()
    }
}

/// 延时操作数上限：30 天。超限在编译期直接拒绝，防止毒定义持久化。
const MAX_DELAY_SECONDS: i64 = 30 * 24 * 60 * 60;

pub(crate) fn duration_to_seconds(duration: &str) -> Result<i64> {
    if duration.len() < 2 {
        return Err(HookError::Message(format!("invalid duration: {duration}")));
    }
    // 末位单位必须按字符边界截取：毒 AST/毒输入可能携带多字节 UTF-8 结尾
    // （如 "1ü"，编译 cloud AST 时 rawDuration 来自外部 JSON），按字节
    // split_at 会在非边界处 panic——这里取最后一个 char，非 ASCII 单位字母
    // 一律返回确定性错误（有界失败，绝不 panic）。
    let (num, unit) = match duration.char_indices().next_back() {
        Some((index, unit)) if unit.is_ascii() => (&duration[..index], unit),
        _ => return Err(HookError::Message(format!("invalid duration: {duration}"))),
    };
    // 数值段必须是纯 ASCII 数字：Rust 的 i64::from_str 接受前导 '+'（毒
    // AST 的 rawDuration 可携带 "+5s"），解析器产出的 duration 永不带符号，
    // 解码侧按严格格式拒绝，两侧输入在同一口径下收敛。
    if num.is_empty() || !num.chars().all(|ch| ch.is_ascii_digit()) {
        return Err(HookError::Message(format!("invalid duration: {duration}")));
    }
    if num.starts_with('0') {
        return Err(HookError::Message(format!("invalid duration: {duration}")));
    }
    let value = num
        .parse::<i64>()
        .map_err(|err| HookError::Message(format!("invalid duration {duration}: {err}")))?;
    let multiplier = match unit {
        's' => 1,
        'm' => 60,
        'h' => 60 * 60,
        'd' => 60 * 60 * 24,
        _ => return Err(HookError::Message(format!("invalid duration unit: {unit}"))),
    };
    let seconds = value
        .checked_mul(multiplier)
        .ok_or_else(|| HookError::Message(format!("duration is too large: {duration}")))?;
    if seconds > MAX_DELAY_SECONDS {
        return Err(HookError::Message(format!(
            "duration {duration} exceeds the maximum allowed delay of {MAX_DELAY_SECONDS}s (30d)"
        )));
    }
    Ok(seconds)
}

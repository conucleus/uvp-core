//! Zhixu Dock 统一委托协议（PRD_100/PRD_102）的语义权威实现。
//!
//! 本模块固定：
//! - 目标 `spec.dockInterface`（具名接口 map）与调用方
//!   `executor.zhixuExecutorConfig`（键闭集 {target, interface, order,
//!   inputMap, signalMap}）的 source 语义；
//! - 跨定义 linker（resolution manifest v2 输入，纯函数，无网络；内嵌
//!   目标定义全文，按内容派生身份三方一致校验）；
//! - 全部跨运行时 commitment 的 keccak/ABI-word 编码（v2 word 布局见
//!   PRD100_102_DESIGN.md §8）、Merkle root、`dockInstanceId`/
//!   `linkedOrderId` 推导与 envelope 幂等键；
//! - 编译期错误码 D001-D016、D018-D020、D025、接口形状错误码 D021/D022。
//!
//! 哈希规则（Rust/TS/Solidity/Go 必须逐字节一致）：
//! - 所有 commitment 哈希 = `keccak256(domainWord ‖ w1 ‖ … ‖ wn)`，其中
//!   `domainWord = keccak256("<DOMAIN>")`，`wi` 为 32-byte word。这与
//!   Solidity `keccak256(abi.encode(keccak256("<DOMAIN>"), …))` 完全一致
//!   （全部字段均为 word，无动态类型，禁 encodePacked）。
//! - Merkle：叶子为 word；空集合 root = `keccak256("")`
//!   （`EMPTY_MERKLE_ROOT`）；配对合并 `keccak256(min(a,b) ‖ max(a,b))`
//!   （字节序取小者为左）；叶子列表先按字节升序排序再建树。
//! - 枚举 word：route modeWord new=0/existing=1；接口 orderModesWord
//!   u8 位掩码 bit0=new、bit1=existing。派生 linked order 的最高位是
//!   dock 专用 namespace 标记；普通 MINT/trigger-order 路径必须拒绝该
//!   namespace，以免公开的确定性 linkedOrderId 被抢先注册。

use serde_json::{json, Map, Value};
use sha3::{Digest, Keccak256};
use std::collections::{BTreeMap, BTreeSet, VecDeque};
use uvp_hook_dsl::{parse_hook, DependencyKind, HookMode, ParseHookRequest, Profile};
use uvp_model::{DockInterfaceSpec, ZhixuStage};

pub type Word = [u8; 32];

// ---------------------------------------------------------------------------
// 冻结常量
// ---------------------------------------------------------------------------

pub const DOCK_INTERFACE_ARTIFACT_SCHEMA_VERSION: &str = "uvp.dockInterfaceArtifact.v2";
pub const DOCK_ROUTE_SCHEMA_VERSION: &str = "uvp.dockRoute.v2";
/// 未解析 route（target:null 动态选择）的声明面产物形态：本地声明完整、
/// 目标身份空缺，云轨运行时由选择记录补齐（PRD_100 §10.3、设计文档 §8.8）。
pub const DOCK_ROUTE_UNRESOLVED_SCHEMA_VERSION: &str = "uvp.dockRoute.unresolved.v1";
pub const DOCK_RESOLUTION_SCHEMA_VERSION: &str = "uvp.dock.resolution.v2";
pub const DOCK_COMPAT_SCHEMA_VERSION: &str = "uvp.dock.compat.v1";

pub const MAX_DOCK_INPUTS: usize = 8;
pub const MAX_DOCK_OUTPUTS: usize = 16;
/// Maximum number of definitions in a statically linked startup path. Runtime
/// adapters must enforce the same limit against the actual parent instance
/// depth as well; this linker check cannot observe runtime-created orders.
pub const MAX_DOCK_DEPTH: u8 = 8;
/// `^[a-z][a-z0-9_]{0,31}$`：端口名与接口名同规则（PRD_100 §9.3）。
pub const MAX_PORT_NAME_BYTES: usize = 32;

/// signalMap key 上限：运行期 hook 命名空间 = "signalMap." + key（10 字节
/// 前缀）而 hook_name 列宽 36 ⇒ key 上限 26。与 Go 镜像统一口径，避免
/// 27-36 字节 key 在一侧收、另一侧放的分裂。
pub const MAX_SIGNAL_MAP_KEY_LENGTH: usize = 26;

/// canonical 三段式信号名（task.stage.signal）落
/// individual_record.signal_name / hook_dependency.signal_name 的列宽。
/// stage 标识符 + "." + signalMap key 的组合长度按同值钉死。
pub const MAX_SIGNAL_NAME_BYTES: usize = 100;

/// order.mode 与接口 orderModes 的闭集取值（PRD_100 §11）。
pub const ORDER_MODE_NEW: &str = "new";
pub const ORDER_MODE_EXISTING: &str = "existing";
pub const ORDER_MODES: [&str; 2] = [ORDER_MODE_NEW, ORDER_MODE_EXISTING];

pub const DOMAIN_DEFINITION_REF: &str = "UVP_DEFINITION_REF_V1";
pub const DOMAIN_INTERFACE: &str = "UVP_DOCK_INTERFACE_V2";
pub const DOMAIN_INTERFACE_INPUT: &str = "UVP_DOCK_INTERFACE_INPUT_V2";
pub const DOMAIN_INTERFACE_OUTPUT: &str = "UVP_DOCK_INTERFACE_OUTPUT_V2";
pub const DOMAIN_ROUTE_ID: &str = "UVP_DOCK_ROUTE_ID_V1";
pub const DOMAIN_INPUT_BINDING: &str = "UVP_DOCK_INPUT_BINDING_V2";
pub const DOMAIN_OUTPUT_BINDING: &str = "UVP_DOCK_OUTPUT_BINDING_V2";
pub const DOMAIN_ROUTE: &str = "UVP_DOCK_ROUTE_V2";
pub const DOMAIN_DOCK_INSTANCE: &str = "UVP_DOCK_INSTANCE_V2";
pub const DOMAIN_DOCK_ORDER: &str = "UVP_DOCK_ORDER_V1";
/// Highest bit reserved for deterministically-derived dock child orders.
/// Keeping the namespace bit outside the hash preimage preserves the
/// commitment inputs while making public MINT order creation disjoint from
/// dock creation.
pub const DOCK_ORDER_NAMESPACE_MASK: u8 = 0x80;
pub const DOMAIN_RUNTIME_EIP155: &str = "UVP_RUNTIME_EIP155_V1";
pub const DOMAIN_RUNTIME_CLOUD: &str = "UVP_RUNTIME_CLOUD_V1";
pub const DOMAIN_INPUT_PAYLOAD: &str = "UVP_DOCK_INPUT_PAYLOAD_V1";
pub const DOMAIN_INPUT_IDEMPOTENCY: &str = "UVP_DOCK_INPUT_IDEMPOTENCY_V1";
pub const DOMAIN_OUTPUT_IDEMPOTENCY: &str = "UVP_DOCK_OUTPUT_IDEMPOTENCY_V1";
pub const DOMAIN_SOURCE_FACT_SET: &str = "UVP_DOCK_SOURCE_FACT_SET_V1";

/// 空 Merkle root：`keccak256("")`。
pub const EMPTY_MERKLE_ROOT: Word = [
    0xc5, 0xd2, 0x46, 0x01, 0x86, 0xf7, 0x23, 0x3c, 0x92, 0x7e, 0x7d, 0xb2, 0xdc, 0xc7, 0x03, 0xc0,
    0xe5, 0x00, 0xb6, 0x53, 0xca, 0x82, 0x27, 0x3b, 0x7b, 0xfa, 0xd8, 0x04, 0x5d, 0x85, 0xa4, 0x70,
];

// ---------------------------------------------------------------------------
// 哈希原语
// ---------------------------------------------------------------------------

pub fn keccak_word(data: &[u8]) -> Word {
    let mut out = [0u8; 32];
    out.copy_from_slice(&Keccak256::digest(data));
    out
}

pub fn word_hex(word: &Word) -> String {
    let mut out = String::with_capacity(66);
    out.push_str("0x");
    for byte in word {
        out.push_str(&format!("{byte:02x}"));
    }
    out
}

fn word_from_hex(value: &str) -> Option<Word> {
    let body = value.strip_prefix("0x")?;
    if body.len() != 64 {
        return None;
    }
    let mut out = [0u8; 32];
    for (index, chunk) in body.as_bytes().chunks(2).enumerate() {
        let high = (chunk[0] as char).to_digit(16)?;
        let low = (chunk[1] as char).to_digit(16)?;
        out[index] = ((high << 4) | low) as u8;
    }
    Some(out)
}

/// `keccak256(keccak256(domain) ‖ words…)`，等价于 Solidity
/// `keccak256(abi.encode(keccak256(domain), …words))`。
pub fn keccak_words(domain: &str, words: &[Word]) -> Word {
    let mut buf = Vec::with_capacity(32 * (words.len() + 1));
    buf.extend_from_slice(&keccak_word(domain.as_bytes()));
    for word in words {
        buf.extend_from_slice(word);
    }
    keccak_word(&buf)
}

fn u64_word(value: u64) -> Word {
    let mut out = [0u8; 32];
    out[24..].copy_from_slice(&value.to_be_bytes());
    out
}

fn u8_word(value: u8) -> Word {
    let mut out = [0u8; 32];
    out[31] = value;
    out
}

fn address_word(address: &str) -> Option<Word> {
    let body = address.strip_prefix("0x")?;
    if body.len() != 40 || !body.chars().all(|c| c.is_ascii_hexdigit()) {
        return None;
    }
    let mut out = [0u8; 32];
    for (index, chunk) in body.as_bytes().chunks(2).enumerate() {
        let high = (chunk[0] as char).to_digit(16)? as u8;
        let low = (chunk[1] as char).to_digit(16)? as u8;
        out[12 + index] = (high << 4) | low;
    }
    Some(out)
}

/// route 的 order mode word：new=0、existing=1（PRD_100 §11）。
pub fn mode_word(mode: &str) -> Option<Word> {
    match mode {
        ORDER_MODE_NEW => Some(u8_word(0)),
        ORDER_MODE_EXISTING => Some(u8_word(1)),
        _ => None,
    }
}

/// 接口 orderModes word：u8 位掩码，bit0=new、bit1=existing；未知取值或
/// 重复项返回 None（由调用方按 D025/D008 上报）。
pub fn order_modes_word(modes: &[String]) -> Option<Word> {
    let mut mask = 0u8;
    let mut seen = BTreeSet::new();
    for mode in modes {
        if !seen.insert(mode.as_str()) {
            return None;
        }
        match mode.as_str() {
            ORDER_MODE_NEW => mask |= 0b01,
            ORDER_MODE_EXISTING => mask |= 0b10,
            _ => return None,
        }
    }
    if mask == 0 {
        return None;
    }
    Some(u8_word(mask))
}

/// 排序配对 Merkle root。叶子先按字节升序去重排序，逐层
/// `keccak256(min ‖ max)` 合并；空集合返回 `EMPTY_MERKLE_ROOT`。
pub fn merkle_root(leaves: &[Word]) -> Word {
    if leaves.is_empty() {
        return EMPTY_MERKLE_ROOT;
    }
    let mut level: Vec<Word> = leaves.to_vec();
    level.sort_unstable();
    level.dedup();
    while level.len() > 1 {
        let mut next = Vec::with_capacity(level.len() / 2 + 1);
        let mut index = 0;
        while index < level.len() {
            if index + 1 == level.len() {
                // 奇数层：最后一个叶子提升一层（不与自身合并）。
                next.push(level[index]);
            } else {
                let (left, right) = if level[index] <= level[index + 1] {
                    (level[index], level[index + 1])
                } else {
                    (level[index + 1], level[index])
                };
                let mut buf = [0u8; 64];
                buf[..32].copy_from_slice(&left);
                buf[32..].copy_from_slice(&right);
                next.push(keccak_word(&buf));
            }
            index += 2;
        }
        level = next;
    }
    level[0]
}

/// Merkle inclusion proof（供 TS/Solidity 测试对齐；core 自身仅需要 root）。
pub fn merkle_proof(leaves: &[Word], leaf: &Word) -> Option<Vec<Word>> {
    if leaves.is_empty() || !leaves.contains(leaf) {
        return None;
    }
    let mut level: Vec<Word> = {
        let mut sorted = leaves.to_vec();
        sorted.sort_unstable();
        sorted.dedup();
        sorted
    };
    let mut index = level.iter().position(|candidate| candidate == leaf)?;
    let mut proof = Vec::new();
    while level.len() > 1 {
        let mut next = Vec::with_capacity(level.len() / 2 + 1);
        let mut cursor = 0;
        while cursor < level.len() {
            if cursor + 1 == level.len() {
                next.push(level[cursor]);
                if cursor == index {
                    index = next.len() - 1;
                }
            } else {
                let (left, right) = if level[cursor] <= level[cursor + 1] {
                    (level[cursor], level[cursor + 1])
                } else {
                    (level[cursor + 1], level[cursor])
                };
                let mut buf = [0u8; 64];
                buf[..32].copy_from_slice(&left);
                buf[32..].copy_from_slice(&right);
                next.push(keccak_word(&buf));
                if cursor == index {
                    proof.push(level[cursor + 1]);
                    index = next.len() - 1;
                } else if cursor + 1 == index {
                    proof.push(level[cursor]);
                    index = next.len() - 1;
                }
            }
            cursor += 2;
        }
        level = next;
    }
    Some(proof)
}

// ---------------------------------------------------------------------------
// 身份推导（PRD_102 §5、PRD_100 §13）
// ---------------------------------------------------------------------------

/// `definitionRefHash = H("UVP_DEFINITION_REF_V1", keccak(uid))`
pub fn definition_ref_hash(uid: &str) -> Word {
    keccak_words(DOMAIN_DEFINITION_REF, &[keccak_word(uid.as_bytes())])
}

pub fn stage_key(stage_identifier: &str) -> Word {
    keccak_word(stage_identifier.as_bytes())
}

pub fn hook_key(hook_id: &str) -> Word {
    keccak_word(hook_id.as_bytes())
}

pub fn port_key(port_name: &str) -> Word {
    keccak_word(port_name.as_bytes())
}

/// 接口名在全部 v2 preimage 中的 word 形态：`keccak256(utf8(name))`。
pub fn interface_name_key(interface_name: &str) -> Word {
    keccak_word(interface_name.as_bytes())
}

pub fn canonical_signal_hash(canonical: &str) -> Word {
    keccak_word(canonical.as_bytes())
}

/// StateMachine 事实寻址键：`keccak256(abi.encode(sourceId, signalId))`
/// （64 字节拼接，无 domain）。output 幂等键的 targetFactId 在链上只能
/// 从 word 推导，采用本键。
pub fn signal_key(source_id: &Word, signal_id: &Word) -> Word {
    let mut buf = [0u8; 64];
    buf[..32].copy_from_slice(source_id);
    buf[32..].copy_from_slice(signal_id);
    keccak_word(&buf)
}

pub fn route_id(local_definition_ref: &Word, stage_key_word: &Word) -> Word {
    keccak_words(DOMAIN_ROUTE_ID, &[*local_definition_ref, *stage_key_word])
}

/// EVM runtime domain：chainId + StateMachine 地址。
pub fn evm_runtime_domain(chain_id: u64, state_machine_address: &str) -> Option<Word> {
    let address = address_word(state_machine_address)?;
    Some(keccak_words(
        DOMAIN_RUNTIME_EIP155,
        &[u64_word(chain_id), address],
    ))
}

/// Cloud runtime domain：deploymentId + securityDomain（须为持久配置）。
pub fn cloud_runtime_domain(deployment_id: &str, security_domain: &str) -> Word {
    keccak_words(
        DOMAIN_RUNTIME_CLOUD,
        &[
            keccak_word(deployment_id.as_bytes()),
            keccak_word(security_domain.as_bytes()),
        ],
    )
}

/// 字符串 orderID 先哈希成 word（入口 API 固定派生规则）。
pub fn local_order_key(order_id: &str) -> Word {
    keccak_word(order_id.as_bytes())
}

/// existing 模式的目标 order 引用（运行系统作用域内解析的唯一引用，
/// PRD_100 §11.4）在 dockInstanceId preimage 中的 word 形态。
pub fn target_order_ref_key(order_ref: &str) -> Word {
    keccak_word(order_ref.as_bytes())
}

/// dockInstanceId v2：new 模式 8 word（幂等建单锚），existing 模式在尾部
/// 追加第 9 个 word = target order 引用（引用不同即不同实例）。
#[allow(clippy::too_many_arguments)]
pub fn dock_instance_id(
    runtime_domain: &Word,
    local_plan_id: &Word,
    local_definition_ref: &Word,
    local_order_key: &Word,
    route_id_word: &Word,
    route_hash: &Word,
    mode_word: &Word,
    interface_name_hash: &Word,
    target_order_ref: Option<&Word>,
) -> Word {
    let mut words = vec![
        *runtime_domain,
        *local_plan_id,
        *local_definition_ref,
        *local_order_key,
        *route_id_word,
        *route_hash,
        *mode_word,
        *interface_name_hash,
    ];
    if let Some(order_ref) = target_order_ref {
        words.push(*order_ref);
    }
    keccak_words(DOMAIN_DOCK_INSTANCE, &words)
}

pub fn linked_order_id(dock_instance_id: &Word, target_definition_ref: &Word) -> Word {
    let mut linked = keccak_words(
        DOMAIN_DOCK_ORDER,
        &[*dock_instance_id, *target_definition_ref],
    );
    linked[0] |= DOCK_ORDER_NAMESPACE_MASK;
    linked
}

/// Dock input envelope payload hash。全部字段 word 化；target 信号词使用
/// StateMachine 事实寻址的 signalId（keccak(task.stage.signal)），与
/// UVPDockingModule._inputPayloadHash 逐字一致。
#[allow(clippy::too_many_arguments)]
pub fn dock_input_payload_hash(
    dock_instance: &Word,
    route_hash: &Word,
    local_plan_id: &Word,
    local_order: &Word,
    local_stage_key: &Word,
    local_hook_key: &Word,
    target_plan_id: &Word,
    linked_order: &Word,
    target_port_key: &Word,
    target_signal_id: &Word,
    sequence: u64,
) -> Word {
    keccak_words(
        DOMAIN_INPUT_PAYLOAD,
        &[
            *dock_instance,
            *route_hash,
            *local_plan_id,
            *local_order,
            *local_stage_key,
            *local_hook_key,
            *target_plan_id,
            *linked_order,
            *target_port_key,
            *target_signal_id,
            u64_word(sequence),
            [0u8; 32],
        ],
    )
}

/// `sourceFactSetHash = H("UVP_DOCK_SOURCE_FACT_SET_V1", n, w1..wn)`，
/// fact word 列表须按稳定顺序（canonical signal hash 升序）提供。
pub fn source_fact_set_hash(fact_words: &[Word]) -> Word {
    let mut words = Vec::with_capacity(fact_words.len() + 1);
    words.push(u64_word(fact_words.len() as u64));
    words.extend_from_slice(fact_words);
    keccak_words(DOMAIN_SOURCE_FACT_SET, &words)
}

pub fn dock_input_idempotency_key(
    dock_instance: &Word,
    input_binding_hash: &Word,
    local_hook_ready_occurrence: u64,
) -> Word {
    keccak_words(
        DOMAIN_INPUT_IDEMPOTENCY,
        &[
            *dock_instance,
            *input_binding_hash,
            u64_word(local_hook_ready_occurrence),
        ],
    )
}

pub fn dock_output_idempotency_key(
    dock_instance: &Word,
    output_binding_hash: &Word,
    target_fact_id: &Word,
) -> Word {
    keccak_words(
        DOMAIN_OUTPUT_IDEMPOTENCY,
        &[*dock_instance, *output_binding_hash, *target_fact_id],
    )
}

/// N6 显示口径（PRD_102 §4）：`name(uid 去 zx- 后前 8 hex)`。name 只是
/// 提示，uid 才是解析键；报错/日志在 name 可得时按此渲染。
pub fn display_identity(name: &str, uid: &str) -> String {
    let hex = uid.strip_prefix("zx-").unwrap_or(uid);
    let short = &hex[..hex.len().min(8)];
    format!("{name}({short})")
}

// ---------------------------------------------------------------------------
// 错误模型
// ---------------------------------------------------------------------------

#[derive(Debug, Clone)]
pub struct DockIssue {
    pub code: &'static str,
    pub path: String,
    pub message: String,
}

impl DockIssue {
    fn new(code: &'static str, path: impl Into<String>, message: impl Into<String>) -> Self {
        Self {
            code,
            path: path.into(),
            message: message.into(),
        }
    }
}

impl std::fmt::Display for DockIssue {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{} {}: {}", self.code, self.path, self.message)
    }
}

pub type DockResult<T> = std::result::Result<T, Vec<DockIssue>>;

const UNSUPPORTED_HINT: &str = "Zhixu delegation binds a named target interface: publish target \
    spec.dockInterface {<interface>: {orderModes, inputs, outputs}} and bind \
    executor.zhixuExecutorConfig {target, interface, order.mode, inputMap, signalMap-to-port-names}; \
    re-link and republish, do not expect runtime compatibility";

// ---------------------------------------------------------------------------
// 调用方 executor config（source 层）
// ---------------------------------------------------------------------------

#[derive(Debug, Clone)]
pub struct ZhixuExecutorConfig {
    /// `None` = `target: null`（动态选择，运行时由选择记录补齐）。
    pub target_zhixu: Option<String>,
    pub interface_name: String,
    /// `new` | `existing`。
    pub order_mode: String,
    pub input_map: BTreeMap<String, String>,
    pub signal_map: BTreeMap<String, String>,
}

/// 目标定义派生身份的文本形态：`zx-` + 恰好 32 个小写 hex 字符。
pub fn valid_derived_uid(uid: &str) -> bool {
    let Some(hex) = uid.strip_prefix("zx-") else {
        return false;
    };
    hex.len() == 32
        && hex
            .chars()
            .all(|c| c.is_ascii_digit() || ('a'..='f').contains(&c))
}

/// 解析并本地校验 `executor.zhixuExecutorConfig`（D001-D006、D010、D019）。
/// `stage` 为该 executor 所属 stage；`path` 为报错 JSON path 前缀。
pub fn parse_zhixu_executor_config(
    executor_value: &Value,
    stage: &ZhixuStage,
    stage_identifier: &str,
) -> DockResult<ZhixuExecutorConfig> {
    let path = format!("{stage_identifier}.executor.zhixuExecutorConfig");
    let mut issues = Vec::new();

    // D001：zhixu executor 禁止 supplierID。
    if executor_value
        .get("supplierID")
        .and_then(Value::as_str)
        .is_some_and(|id| !id.trim().is_empty())
    {
        issues.push(DockIssue::new(
            "D001",
            format!("{stage_identifier}.executor.supplierID"),
            "supplierID is forbidden when supplierType is zhixu; the target identity must live in zhixuExecutorConfig.target",
        ));
    }

    let Some(config) = executor_value.get("zhixuExecutorConfig") else {
        issues.push(DockIssue::new(
            "D002",
            &path,
            "zhixuExecutorConfig is required when supplierType is zhixu",
        ));
        return Err(issues);
    };
    let Some(config_object) = config.as_object() else {
        issues.push(DockIssue::new(
            "D002",
            &path,
            "zhixuExecutorConfig must be an object",
        ));
        return Err(issues);
    };

    // D002：键闭集（schemaVersion 等残留键 = 未知字段硬错误）。
    const ALLOWED_KEYS: [&str; 5] = ["target", "interface", "order", "inputMap", "signalMap"];
    for key in config_object.keys() {
        if !ALLOWED_KEYS.contains(&key.as_str()) {
            issues.push(DockIssue::new(
                "D002",
                format!("{path}.{key}"),
                format!("unknown field {key:?}; allowed: {ALLOWED_KEYS:?}"),
            ));
        }
    }

    // D003：target 必填键——{zhixu: <派生uid>} 或显式 null（动态选择）。
    let target_zhixu = match config_object.get("target") {
        None => {
            issues.push(DockIssue::new(
                "D003",
                format!("{path}.target"),
                "target is required: {zhixu: <derived uid>} for a static target, or null for runtime selection",
            ));
            None
        }
        Some(Value::Null) => None,
        Some(Value::Object(target)) => {
            for key in target.keys() {
                if key != "zhixu" {
                    issues.push(DockIssue::new(
                        "D002",
                        format!("{path}.target.{key}"),
                        format!("unknown field {key:?}; allowed: [\"zhixu\"]"),
                    ));
                }
            }
            match target.get("zhixu").and_then(Value::as_str) {
                Some(uid) => {
                    let uid = uid.trim();
                    if !valid_derived_uid(uid) {
                        issues.push(DockIssue::new(
                            "D003",
                            format!("{path}.target.zhixu"),
                            "target.zhixu must be the target definition's derived identity zx-<32hex>; {UNSUPPORTED_HINT}",
                        ));
                    }
                    Some(uid.to_string())
                }
                None => {
                    issues.push(DockIssue::new(
                        "D003",
                        format!("{path}.target.zhixu"),
                        "target.zhixu is required when target is an object (the target definition's derived identity)",
                    ));
                    None
                }
            }
        }
        Some(_) => {
            issues.push(DockIssue::new(
                "D003",
                format!("{path}.target"),
                "target must be an object {zhixu: <derived uid>} or null (dynamic selection)",
            ));
            None
        }
    };

    // D002：interface 必填，接口名与端口名同规则。
    let interface_name = config_object
        .get("interface")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .trim()
        .to_string();
    if interface_name.is_empty() {
        issues.push(DockIssue::new(
            "D002",
            format!("{path}.interface"),
            "interface is required (the target interface name)",
        ));
    } else if !valid_port_name(&interface_name) {
        issues.push(DockIssue::new(
            "D002",
            format!("{path}.interface"),
            format!(
                "interface must match ^[a-z][a-z0-9_]{{0,31}}$, found {interface_name:?}; {UNSUPPORTED_HINT}"
            ),
        ));
    }

    // D004：order.mode 闭集 {new, existing}。
    let order_mode = match config_object.get("order") {
        Some(Value::Object(order)) => {
            for key in order.keys() {
                if key != "mode" {
                    issues.push(DockIssue::new(
                        "D002",
                        format!("{path}.order.{key}"),
                        format!("unknown field {key:?}; allowed: [\"mode\"]"),
                    ));
                }
            }
            order
                .get("mode")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .trim()
                .to_string()
        }
        _ => {
            issues.push(DockIssue::new(
                "D004",
                format!("{path}.order.mode"),
                "order.mode is required and must be \"new\" or \"existing\"",
            ));
            String::new()
        }
    };
    if !issues
        .iter()
        .any(|issue| issue.path == format!("{path}.order.mode"))
        && !ORDER_MODES.contains(&order_mode.as_str())
    {
        issues.push(DockIssue::new(
            "D004",
            format!("{path}.order.mode"),
            format!("must be \"new\" or \"existing\", found {order_mode:?}"),
        ));
    }

    let mut parse_map = |key: &str, code: &'static str| -> Option<Map<String, Value>> {
        match config_object.get(key) {
            None => Some(Map::new()),
            Some(Value::Object(map)) => Some(map.clone()),
            Some(_) => {
                issues.push(DockIssue::new(
                    code,
                    format!("{path}.{key}"),
                    format!("{key} must be an object mapping local channels to target port names"),
                ));
                None
            }
        }
    };
    let input_map = parse_map("inputMap", "D005").unwrap_or_default();
    let signal_map = parse_map("signalMap", "D006").unwrap_or_default();

    // D005：inputMap key 必须是本地 receiveSignals 通道；value 必须是合法端口名。
    let mut parsed_input = BTreeMap::new();
    for (hook_name, port) in &input_map {
        if !stage.receive_signals.contains_key(hook_name) {
            issues.push(DockIssue::new(
                "D005",
                format!("{path}.inputMap.{hook_name}"),
                format!("key is not a receiveSignals channel of stage {stage_identifier}"),
            ));
            continue;
        }
        let Some(port_name) = port.as_str() else {
            issues.push(DockIssue::new(
                "D005",
                format!("{path}.inputMap.{hook_name}"),
                "value must be a target input port name string",
            ));
            continue;
        };
        if !valid_port_name(port_name) {
            issues.push(DockIssue::new(
                "D005",
                format!("{path}.inputMap.{hook_name}"),
                format!(
                    "value must be a port name matching ^[a-z][a-z0-9_]{{0,31}}$, found {port_name:?}; {UNSUPPORTED_HINT}"
                ),
            ));
            continue;
        }
        parsed_input.insert(hook_name.clone(), port_name.to_string());
    }
    // 同一目标端口在一次 inputMap 中只能绑定一次。
    let mut ports_seen = BTreeSet::new();
    for (hook_name, port) in &parsed_input {
        if !ports_seen.insert(port.clone()) {
            issues.push(DockIssue::new(
                "D005",
                format!("{path}.inputMap.{hook_name}"),
                format!("target input port {port:?} is bound more than once in this route"),
            ));
        }
    }

    // D006：signalMap key 必须是本地 send signal。key 同时是运行期 hook
    // 命名空间：'.' 是信号名分隔符、组合长度受 signal_name 列宽约束
    // （与 Go 镜像 zhixu_schema.go 同款校验）。
    let mut parsed_signal = BTreeMap::new();
    for (signal_name, port) in &signal_map {
        if signal_name.contains('.') || signal_name.len() > MAX_SIGNAL_MAP_KEY_LENGTH {
            issues.push(DockIssue::new(
                "D006",
                format!("{path}.signalMap.{signal_name}"),
                format!(
                    "key must not contain '.' and must be at most {MAX_SIGNAL_MAP_KEY_LENGTH} bytes"
                ),
            ));
            continue;
        }
        if stage_identifier.len() + 1 + signal_name.len() > MAX_SIGNAL_NAME_BYTES {
            issues.push(DockIssue::new(
                "D006",
                format!("{path}.signalMap.{signal_name}"),
                format!(
                    "stage {stage_identifier:?} combined signal name is {} bytes, exceeds {MAX_SIGNAL_NAME_BYTES} (individual_record.signal_name)",
                    stage_identifier.len() + 1 + signal_name.len()
                ),
            ));
            continue;
        }
        if !stage.send_signals.contains(signal_name) {
            issues.push(DockIssue::new(
                "D006",
                format!("{path}.signalMap.{signal_name}"),
                format!("key is not a sendSignals signal of stage {stage_identifier}"),
            ));
            continue;
        }
        let Some(port_name) = port.as_str() else {
            issues.push(DockIssue::new(
                "D006",
                format!("{path}.signalMap.{signal_name}"),
                "value must be a target output port name string",
            ));
            continue;
        };
        if !valid_port_name(port_name) {
            issues.push(DockIssue::new(
                "D006",
                format!("{path}.signalMap.{signal_name}"),
                format!(
                    "value must be a port name matching ^[a-z][a-z0-9_]{{0,31}}$, found {port_name:?}; {UNSUPPORTED_HINT}"
                ),
            ));
            continue;
        }
        parsed_signal.insert(signal_name.clone(), port_name.to_string());
    }
    let mut output_ports_seen = BTreeSet::new();
    for (signal_name, port) in &parsed_signal {
        if !output_ports_seen.insert(port.clone()) {
            issues.push(DockIssue::new(
                "D006",
                format!("{path}.signalMap.{signal_name}"),
                format!("target output port {port:?} is bound more than once in this route"),
            ));
        }
    }

    // D019：至少声明一项输入或输出映射（PRD_100 §10.1）。
    if parsed_input.is_empty() && parsed_signal.is_empty() {
        issues.push(DockIssue::new(
            "D019",
            &path,
            "at least one of inputMap/signalMap must bind a port (a route maps an input or an output; business str/cmp are not required)",
        ));
    }

    // D010：new 模式恰好一条 input 绑定（建单入口需要确定的出生锚）。
    if order_mode == ORDER_MODE_NEW && parsed_input.len() != 1 {
        issues.push(DockIssue::new(
            "D010",
            format!("{path}.inputMap"),
            format!(
                "order.mode new requires exactly one inputMap binding (the birth anchor), found {}",
                parsed_input.len()
            ),
        ));
    }

    if !issues.is_empty() {
        return Err(issues);
    }
    Ok(ZhixuExecutorConfig {
        target_zhixu,
        interface_name,
        order_mode,
        input_map: parsed_input,
        signal_map: parsed_signal,
    })
}

pub fn valid_port_name(name: &str) -> bool {
    let bytes = name.as_bytes();
    if bytes.is_empty() || bytes.len() > MAX_PORT_NAME_BYTES {
        return false;
    }
    if !bytes[0].is_ascii_lowercase() {
        return false;
    }
    bytes[1..]
        .iter()
        .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || *b == b'_')
}

fn is_zhixu_executor(executor: &Option<uvp_model::ZhixuExecutor>) -> bool {
    executor
        .as_ref()
        .is_some_and(|e| e.supplier_type.trim() == "zhixu")
}

/// 未链接 route：本地编译产物（调用方侧）。
#[derive(Debug, Clone)]
pub struct UnlinkedDockRoute {
    pub stage_identifier: String,
    pub stage_key: Word,
    /// 本地 stage source：运行时重算 output 绑定 localSourceId 的输入
    /// （keccak(stage.source)），未解析 route 必须随声明面携带。
    pub stage_source: String,
    pub config: ZhixuExecutorConfig,
}

/// 收集并本地校验一个定义内全部 zhixu executor route（不解析目标端口）。
pub fn collect_unlinked_routes(
    entries: &[(String, ZhixuStage)],
) -> DockResult<Vec<UnlinkedDockRoute>> {
    let mut routes = Vec::new();
    let mut issues = Vec::new();
    for (stage_identifier, stage) in entries {
        let Some(executor) = &stage.executor else {
            continue;
        };
        if !is_zhixu_executor(&stage.executor) {
            continue;
        }
        let executor_value =
            serde_json::to_value(executor).unwrap_or_else(|_| Value::Object(Map::new()));
        match parse_zhixu_executor_config(&executor_value, stage, stage_identifier) {
            Ok(config) => routes.push(UnlinkedDockRoute {
                stage_identifier: stage_identifier.clone(),
                stage_key: stage_key(stage_identifier),
                stage_source: stage.source.clone(),
                config,
            }),
            Err(mut stage_issues) => issues.append(&mut stage_issues),
        }
    }
    if !issues.is_empty() {
        return Err(issues);
    }
    Ok(routes)
}

impl UnlinkedDockRoute {
    /// 未解析 route（target:null）的声明面产物：本地声明完整、目标身份
    /// 空缺（PRD_100 §10.3、设计文档 §8.8）。不携带 routeId/routeHash/
    /// bindingHash——它们的 preimage 含目标定义身份与目标端口寻址 word，
    /// 只能由云轨运行时在选择记录补齐目标后用 v2 派生函数重算。
    /// `localPlanId` 由产物组装层注入（与 resolved route 同一契约）。
    pub fn unresolved_json(&self, local_definition_ref: &Word) -> Value {
        json!({
            "schemaVersion": DOCK_ROUTE_UNRESOLVED_SCHEMA_VERSION,
            "stageIdentifier": self.stage_identifier,
            "stageId": word_hex(&self.stage_key),
            "localDefinitionRefHash": word_hex(local_definition_ref),
            "localSource": self.stage_source,
            "interfaceName": self.config.interface_name,
            "orderMode": self.config.order_mode,
            "inputBindings": self.config.input_map.iter().map(|(hook_name, port)| json!({
                "hookId": format!("{}#{hook_name}", self.stage_identifier),
                "port": port,
            })).collect::<Vec<_>>(),
            "outputBindings": self.config.signal_map.iter().map(|(signal_name, port)| json!({
                "signal": signal_name,
                "port": port,
            })).collect::<Vec<_>>(),
        })
    }
}

// ---------------------------------------------------------------------------
// 目标接口（spec.dockInterface → DockInterfaceArtifact v2）
// ---------------------------------------------------------------------------

#[derive(Debug, Clone)]
pub struct DockInterfaceArtifactPortInput {
    pub port: String,
    pub stage_identifier: String,
    pub hook_name: String,
    /// `<task>.<stage>#<receiveHookName>`
    pub hook_id: String,
    pub canonical_input_signal: String,
    pub canonical_input_signal_hash: Word,
    pub source: String,
    pub source_id: Word,
    pub signal_id: Word,
    pub leaf_hash: Word,
}

#[derive(Debug, Clone)]
pub struct DockInterfaceArtifactPortOutput {
    pub port: String,
    pub canonical_output_signal: String,
    pub canonical_output_signal_hash: Word,
    pub source: String,
    pub source_id: Word,
    pub signal_id: Word,
    pub leaf_hash: Word,
}

/// 一个具名接口的编译产物；`interface_leaf` 即 manifest 里的
/// `interfaceRoot`（该接口对外的完整承诺）。
#[derive(Debug, Clone)]
pub struct InterfaceArtifact {
    pub name: String,
    pub order_modes: Vec<String>,
    pub inputs: Vec<DockInterfaceArtifactPortInput>,
    pub outputs: Vec<DockInterfaceArtifactPortOutput>,
    pub inputs_root: Word,
    pub outputs_root: Word,
    pub interface_leaf: Word,
}

#[derive(Debug, Clone)]
pub struct DockInterfaceArtifact {
    pub uid: String,
    pub definition_ref_hash: Word,
    /// 按接口名升序。
    pub interfaces: Vec<InterfaceArtifact>,
    /// 全部接口叶的 merkle root（无接口 → EMPTY root）。
    pub interface_root: Word,
}

/// 编译目标定义的 `spec.dockInterface`（D013/D014、D021/D022、D025）。
pub fn compile_dock_interface(
    dock: &BTreeMap<String, DockInterfaceSpec>,
    uid: &str,
    entries: &[(String, ZhixuStage)],
) -> DockResult<DockInterfaceArtifact> {
    let mut issues = Vec::new();

    let stages_by_identifier: BTreeMap<&str, &ZhixuStage> = entries
        .iter()
        .map(|(identifier, stage)| (identifier.as_str(), stage))
        .collect();
    // mailbox hook 全定义唯一发布：同一物理入口被两个公开端口重复发布会
    // 让外部投递出现两条可寻址路径。
    let mut hooks_claimed: BTreeMap<String, String> = BTreeMap::new();
    let uid_word = keccak_word(uid.as_bytes());
    let mut interfaces = Vec::new();
    let mut interface_leaves = Vec::new();

    for (interface_name, spec) in dock {
        let base_path = format!("spec.dockInterface.{interface_name}");
        if !valid_port_name(interface_name) {
            issues.push(DockIssue::new(
                "D021",
                &base_path,
                "interface name must match ^[a-z][a-z0-9_]{0,31}$ (same rule as port names)",
            ));
            continue;
        }
        let Some(modes_word) = order_modes_word(&spec.order_modes) else {
            issues.push(DockIssue::new(
                "D025",
                format!("{base_path}.orderModes"),
                "orderModes must be a non-empty subset of {new, existing} without duplicates",
            ));
            continue;
        };
        let name_word = interface_name_key(interface_name);

        let mut inputs = Vec::new();
        let mut outputs = Vec::new();
        let mut input_leaves = Vec::new();
        let mut output_leaves = Vec::new();

        for (port_name, port) in &spec.inputs {
            let path = format!("{base_path}.inputs.{port_name}");
            if !valid_port_name(port_name) {
                issues.push(DockIssue::new(
                    "D021",
                    &path,
                    "port name must match ^[a-z][a-z0-9_]{0,31}$",
                ));
                continue;
            }
            // D022：hook 引用 + 真实存在。
            let Some((stage_identifier, hook_name)) = parse_hook_reference(&port.hook) else {
                issues.push(DockIssue::new(
                    "D022",
                    format!("{path}.hook"),
                    format!(
                        "hook must be <task>.<stage>#<receiveHookName>, found {:?}",
                        port.hook
                    ),
                ));
                continue;
            };
            let Some(stage) = stages_by_identifier.get(stage_identifier.as_str()).copied() else {
                issues.push(DockIssue::new(
                    "D022",
                    format!("{path}.hook"),
                    format!("references unknown stage {stage_identifier}"),
                ));
                continue;
            };
            let Some(raw_expression) = stage.receive_signals.get(&hook_name) else {
                issues.push(DockIssue::new(
                    "D022",
                    format!("{path}.hook"),
                    format!("stage {stage_identifier} has no receiveSignals hook {hook_name}"),
                ));
                continue;
            };
            if let Some(previous) = hooks_claimed.get(&port.hook) {
                issues.push(DockIssue::new(
                    "D022",
                    format!("{path}.hook"),
                    format!("hook {} is already published by port {previous}", port.hook),
                ));
                continue;
            }
            hooks_claimed.insert(port.hook.clone(), format!("{interface_name}.{port_name}"));

            let parsed = match parse_hook(ParseHookRequest {
                profile: Profile::EvmStrict,
                hook_name: hook_name.clone(),
                hook: raw_expression.clone(),
            }) {
                Ok(parsed) => parsed,
                Err(err) => {
                    issues.push(DockIssue::new(
                        "D013",
                        format!("{stage_identifier}.receiveSignals.{hook_name}"),
                        format!("input port hook expression is invalid: {err}"),
                    ));
                    continue;
                }
            };
            // D013：恰好一个正向 canonical signal atom；禁止组合/否定/计时/订阅。
            // atom 信号不要求 ∈ sendSignals——它是 dock 注入的输入事实。
            let single_atom = parsed.mode == HookMode::Normal
                && parsed.dependencies.len() == 1
                && parsed.dependencies[0].kind == DependencyKind::Positive
                && parsed.dependencies[0].delay_seconds.is_none();
            if !single_atom {
                issues.push(DockIssue::new(
                    "D013",
                    format!("{stage_identifier}.receiveSignals.{hook_name}"),
                    "input port hook must be exactly one positive canonical signal atom (no &, |, ~, timers, aggregation, or ANCHOR)",
                ));
                continue;
            }
            let dependency = &parsed.dependencies[0];
            if dependency.source != stage.source {
                issues.push(DockIssue::new(
                    "D013",
                    format!("{stage_identifier}.receiveSignals.{hook_name}"),
                    format!(
                        "input port atom source {} must equal the owning stage source {}",
                        dependency.source, stage.source
                    ),
                ));
                continue;
            }
            // atom 的 (task, stage) 必须落在所属 stage 上：mailbox 地址不可指向别处。
            if !dependency
                .signal_name
                .starts_with(&format!("{stage_identifier}."))
            {
                issues.push(DockIssue::new(
                    "D013",
                    format!("{stage_identifier}.receiveSignals.{hook_name}"),
                    format!(
                        "input port atom must address the owning stage {stage_identifier}, found {}",
                        dependency.signal_name
                    ),
                ));
                continue;
            }

            let canonical_input_signal = format!("{}::{}", stage.source, dependency.signal_name);
            let canonical_hash = canonical_signal_hash(&canonical_input_signal);
            // 叶子只提交对外承诺的 word（uid/接口名/端口名/hook 引用）；
            // sourceId/signalId 是运行期投递寻址数据，随产物携带但不入叶。
            let source_id_word = keccak_word(stage.source.as_bytes());
            let signal_id_word = keccak_word(dependency.signal_name.as_bytes());
            let leaf_hash = keccak_words(
                DOMAIN_INTERFACE_INPUT,
                &[
                    uid_word,
                    name_word,
                    port_key(port_name),
                    hook_key(&port.hook),
                ],
            );
            input_leaves.push(leaf_hash);
            inputs.push(DockInterfaceArtifactPortInput {
                port: port_name.clone(),
                stage_identifier: stage_identifier.clone(),
                hook_name: hook_name.clone(),
                hook_id: port.hook.clone(),
                canonical_input_signal,
                canonical_input_signal_hash: canonical_hash,
                source: stage.source.clone(),
                source_id: source_id_word,
                signal_id: signal_id_word,
                leaf_hash,
            });
        }

        for (port_name, port) in &spec.outputs {
            let path = format!("{base_path}.outputs.{port_name}");
            if !valid_port_name(port_name) {
                issues.push(DockIssue::new(
                    "D021",
                    &path,
                    "port name must match ^[a-z][a-z0-9_]{0,31}$",
                ));
                continue;
            }
            // D014：真实 send capability。
            let Some((source, stage_identifier, signal_name)) =
                parse_canonical_signal(&port.signal)
            else {
                issues.push(DockIssue::new(
                    "D014",
                    format!("{path}.signal"),
                    format!(
                        "signal must be <source>::<task>.<stage>.<signal>, found {:?}",
                        port.signal
                    ),
                ));
                continue;
            };
            let Some(stage) = stages_by_identifier.get(stage_identifier.as_str()).copied() else {
                issues.push(DockIssue::new(
                    "D014",
                    format!("{path}.signal"),
                    format!("references unknown stage {stage_identifier}"),
                ));
                continue;
            };
            if stage.source != source {
                issues.push(DockIssue::new(
                    "D014",
                    format!("{path}.signal"),
                    format!(
                        "signal source {source} must equal stage {stage_identifier} source {}",
                        stage.source
                    ),
                ));
                continue;
            }
            if !stage.send_signals.contains(&signal_name) {
                issues.push(DockIssue::new(
                    "D014",
                    format!("{path}.signal"),
                    format!("signal {signal_name} is not in stage {stage_identifier} sendSignals"),
                ));
                continue;
            }

            let canonical_hash = canonical_signal_hash(&port.signal);
            let source_id_word = keccak_word(source.as_bytes());
            let signal_id_word =
                keccak_word(format!("{stage_identifier}.{signal_name}").as_bytes());
            let leaf_hash = keccak_words(
                DOMAIN_INTERFACE_OUTPUT,
                &[
                    uid_word,
                    name_word,
                    port_key(port_name),
                    canonical_signal_hash(&port.signal),
                ],
            );
            output_leaves.push(leaf_hash);
            outputs.push(DockInterfaceArtifactPortOutput {
                port: port_name.clone(),
                canonical_output_signal: port.signal.clone(),
                canonical_output_signal_hash: canonical_hash,
                source: source.to_string(),
                source_id: source_id_word,
                signal_id: signal_id_word,
                leaf_hash,
            });
        }

        // D025：new ∈ orderModes ⇒ 至少一个 input 端口（建单型服务必须有入口）。
        if spec.order_modes.iter().any(|m| m == ORDER_MODE_NEW) && inputs.is_empty() {
            issues.push(DockIssue::new(
                "D025",
                format!("{base_path}.inputs"),
                "an interface supporting order mode new must expose at least one input port (the birth anchor)",
            ));
            continue;
        }

        let inputs_root = merkle_root(&input_leaves);
        let outputs_root = merkle_root(&output_leaves);
        let interface_leaf = keccak_words(
            DOMAIN_INTERFACE,
            &[uid_word, name_word, modes_word, inputs_root, outputs_root],
        );
        interface_leaves.push(interface_leaf);
        interfaces.push(InterfaceArtifact {
            name: interface_name.clone(),
            order_modes: spec.order_modes.clone(),
            inputs,
            outputs,
            inputs_root,
            outputs_root,
            interface_leaf,
        });
    }

    if !issues.is_empty() {
        return Err(issues);
    }
    // 接口按名排序（BTreeMap 迭代已按名升序，此处显式钉住口径）。
    interfaces.sort_by(|a, b| a.name.cmp(&b.name));
    Ok(DockInterfaceArtifact {
        uid: uid.to_string(),
        definition_ref_hash: definition_ref_hash(uid),
        interfaces,
        interface_root: merkle_root(&interface_leaves),
    })
}

fn parse_hook_reference(reference: &str) -> Option<(String, String)> {
    let (stage_part, hook_name) = reference.split_once('#')?;
    if hook_name.is_empty() || hook_name.contains('#') || hook_name.contains('.') {
        return None;
    }
    let parts: Vec<&str> = stage_part.split('.').collect();
    if parts.len() != 2 || parts[0].is_empty() || parts[1].is_empty() {
        return None;
    }
    Some((stage_part.to_string(), hook_name.to_string()))
}

fn parse_canonical_signal(signal: &str) -> Option<(String, String, String)> {
    let (source, rest) = signal.split_once("::")?;
    let parts: Vec<&str> = rest.split('.').collect();
    if parts.len() != 3 || source.is_empty() {
        return None;
    }
    Some((
        source.to_string(),
        format!("{}.{}", parts[0], parts[1]),
        parts[2].to_string(),
    ))
}

impl DockInterfaceArtifact {
    pub fn to_json(&self) -> Value {
        json!({
            "schemaVersion": DOCK_INTERFACE_ARTIFACT_SCHEMA_VERSION,
            "definition": {
                "uid": self.uid,
                "definitionRefHash": word_hex(&self.definition_ref_hash),
            },
            "interfaces": self.interfaces.iter().map(|interface| json!({
                "name": interface.name,
                "orderModes": interface.order_modes,
                "inputs": interface.inputs.iter().map(|port| json!({
                    "port": port.port,
                    "stageIdentifier": port.stage_identifier,
                    "hookName": port.hook_name,
                    "hookId": port.hook_id,
                    "canonicalInputSignal": port.canonical_input_signal,
                    "canonicalInputSignalHash": word_hex(&port.canonical_input_signal_hash),
                    "source": port.source,
                    "sourceId": word_hex(&port.source_id),
                    "signalId": word_hex(&port.signal_id),
                    "leafHash": word_hex(&port.leaf_hash),
                })).collect::<Vec<_>>(),
                "outputs": interface.outputs.iter().map(|port| json!({
                    "port": port.port,
                    "canonicalOutputSignal": port.canonical_output_signal,
                    "canonicalOutputSignalHash": word_hex(&port.canonical_output_signal_hash),
                    "source": port.source,
                    "sourceId": word_hex(&port.source_id),
                    "signalId": word_hex(&port.signal_id),
                    "leafHash": word_hex(&port.leaf_hash),
                })).collect::<Vec<_>>(),
                "inputsRoot": word_hex(&interface.inputs_root),
                "outputsRoot": word_hex(&interface.outputs_root),
                "interfaceRoot": word_hex(&interface.interface_leaf),
            })).collect::<Vec<_>>(),
            "interfaceRoot": word_hex(&self.interface_root),
        })
    }

    /// 全部 input 端口引用的本地 hook 集合（`<task>.<stage>#<hook>`）：
    /// 这些 mailbox hook 不走普通依赖引用校验。
    pub fn input_port_hook_ids(&self) -> BTreeSet<String> {
        self.interfaces
            .iter()
            .flat_map(|interface| interface.inputs.iter())
            .map(|port| port.hook_id.clone())
            .collect()
    }

    /// 可作为 new 模式出生锚的 input 端口（orderModes 含 new 的接口的全部
    /// input 端口——new 模式 route 的唯一 input 绑定可落在其中任意一个）
    /// 引用的本地 hook 集合，供 orderTriggerKind=dock 标记使用。
    pub fn entrance_hook_ids(&self) -> BTreeSet<String> {
        self.interfaces
            .iter()
            .filter(|interface| interface.order_modes.iter().any(|m| m == ORDER_MODE_NEW))
            .flat_map(|interface| interface.inputs.iter())
            .map(|port| port.hook_id.clone())
            .collect()
    }
}

// ---------------------------------------------------------------------------
// Resolution manifest + linker
// ---------------------------------------------------------------------------

#[derive(Debug, Clone)]
pub struct ResolutionTarget {
    pub zhixu: String,
    /// 内嵌目标定义全文（内容寻址，PRD_102 §5）。
    pub definition_value: Value,
    pub name: String,
    pub definition_ref_hash: Word,
    pub artifact_hash: Word,
    pub published: bool,
    pub interfaces: Vec<InterfaceArtifact>,
    /// 由 interfaces[].interfaceRoot 重算的定义级 dockInterfaceRoot。
    pub dock_interface_root: Word,
    pub cloud_artifact_id: Option<String>,
    pub evm_plan_id: Option<Word>,
    pub dock_edges: Vec<String>,
}

#[derive(Debug, Clone)]
pub struct ResolutionManifest {
    pub targets: Vec<ResolutionTarget>,
}

/// 解析 resolution manifest（Store/发布系统或离线 lock 文件提供）。
/// manifest 只做形状解析；完整性校验（身份重算、root 重算）在 link 时执行。
pub fn parse_resolution_manifest(value: &Value) -> DockResult<ResolutionManifest> {
    let mut issues = Vec::new();
    let schema = value
        .get("schemaVersion")
        .and_then(Value::as_str)
        .unwrap_or_default();
    if schema != DOCK_RESOLUTION_SCHEMA_VERSION {
        issues.push(DockIssue::new(
            "D008",
            "resolutionManifest.schemaVersion",
            format!("must be \"{DOCK_RESOLUTION_SCHEMA_VERSION}\", found {schema:?}"),
        ));
        return Err(issues);
    }
    let mut targets = Vec::new();
    for (index, entry) in value
        .get("definitions")
        .and_then(Value::as_array)
        .unwrap_or(&Vec::new())
        .iter()
        .enumerate()
    {
        let path = format!("resolutionManifest.definitions[{index}]");
        let zhixu = entry
            .get("zhixu")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string();
        if zhixu.is_empty() {
            issues.push(DockIssue::new("D008", &path, "zhixu is required"));
            continue;
        }
        let definition_value = entry.get("definition").cloned().unwrap_or(Value::Null);
        if !definition_value.is_object() {
            issues.push(DockIssue::new(
                "D008",
                format!("{path}.definition"),
                "the full target definition is required (content-addressed identity, PRD_102 §5)",
            ));
            continue;
        }
        let name = definition_value
            .pointer("/metadata/name")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string();
        let parse_word = |key: &str| -> Option<Word> {
            entry
                .get(key)
                .and_then(Value::as_str)
                .and_then(word_from_hex)
        };
        let (Some(definition_ref), Some(artifact_hash)) =
            (parse_word("definitionRefHash"), parse_word("artifactHash"))
        else {
            issues.push(DockIssue::new(
                "D008",
                &path,
                "definitionRefHash and artifactHash must be 0x-prefixed bytes32",
            ));
            continue;
        };
        let published = entry
            .get("published")
            .and_then(Value::as_bool)
            .unwrap_or(false);
        let mut interfaces = Vec::new();
        let mut interfaces_valid = true;
        for (interface_index, interface_value) in entry
            .get("interfaces")
            .and_then(Value::as_array)
            .unwrap_or(&Vec::new())
            .iter()
            .enumerate()
        {
            let interface_path = format!("{path}.interfaces[{interface_index}]");
            match parse_interface_artifact(interface_value) {
                Ok(interface) => interfaces.push(interface),
                Err(mut interface_issues) => {
                    for issue in &mut interface_issues {
                        issue.path = format!("{interface_path}.{}", issue.path);
                    }
                    issues.append(&mut interface_issues);
                    interfaces_valid = false;
                }
            }
        }
        if !interfaces_valid {
            continue;
        }
        if interfaces.is_empty() {
            issues.push(DockIssue::new(
                "D008",
                format!("{path}.interfaces"),
                "target must publish at least one named interface",
            ));
            continue;
        }
        let cloud_artifact_id = entry
            .get("cloudArtifactId")
            .and_then(Value::as_str)
            .map(str::to_string);
        let evm_plan_id = entry
            .get("evmPlanId")
            .and_then(Value::as_str)
            .and_then(word_from_hex);
        let mut dock_edges = Vec::new();
        for edge in entry
            .get("dockEdges")
            .and_then(Value::as_array)
            .unwrap_or(&Vec::new())
        {
            let target_zhixu = edge
                .get("zhixu")
                .and_then(Value::as_str)
                .unwrap_or_default();
            if !target_zhixu.is_empty() {
                dock_edges.push(target_zhixu.to_string());
            }
        }
        let dock_interface_root = merkle_root(
            &interfaces
                .iter()
                .map(|interface| interface.interface_leaf)
                .collect::<Vec<_>>(),
        );
        targets.push(ResolutionTarget {
            zhixu,
            definition_value,
            name,
            definition_ref_hash: definition_ref,
            artifact_hash,
            published,
            interfaces,
            dock_interface_root,
            cloud_artifact_id,
            evm_plan_id,
            dock_edges,
        });
    }
    if !issues.is_empty() {
        return Err(issues);
    }
    Ok(ResolutionManifest { targets })
}

fn parse_interface_artifact(value: &Value) -> DockResult<InterfaceArtifact> {
    let mut issues = Vec::new();
    let name = value
        .get("name")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string();
    if !valid_port_name(&name) {
        issues.push(DockIssue::new(
            "D008",
            "name",
            format!("interface name must match ^[a-z][a-z0-9_]{{0,31}}$, found {name:?}"),
        ));
        return Err(issues);
    }
    let order_modes = value
        .get("orderModes")
        .and_then(Value::as_array)
        .map(|modes| {
            modes
                .iter()
                .filter_map(Value::as_str)
                .map(str::to_string)
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    if order_modes_word(&order_modes).is_none() {
        issues.push(DockIssue::new(
            "D008",
            "orderModes",
            "orderModes must be a non-empty subset of {new, existing} without duplicates",
        ));
        return Err(issues);
    }
    let parse_word = |port: &Value, key: &str| -> Option<Word> {
        port.get(key)
            .and_then(Value::as_str)
            .and_then(word_from_hex)
    };
    let mut inputs = Vec::new();
    let mut outputs = Vec::new();
    for port in value
        .get("inputs")
        .and_then(Value::as_array)
        .unwrap_or(&Vec::new())
    {
        let port_path = || {
            format!(
                "inputs.{}",
                port.get("port").and_then(Value::as_str).unwrap_or("?")
            )
        };
        let Some(leaf_hash) = parse_word(port, "leafHash") else {
            issues.push(DockIssue::new(
                "D008",
                port_path(),
                "leafHash must be bytes32",
            ));
            continue;
        };
        // 悬空/非法的寻址 word 不得静默落成零 word——零 word 会参与
        // leafHash/幂等键的重算比对，只能以确定性错误暴露。
        let (Some(source_id), Some(signal_id)) =
            (parse_word(port, "sourceId"), parse_word(port, "signalId"))
        else {
            issues.push(DockIssue::new(
                "D008",
                port_path(),
                "sourceId/signalId must be 0x-prefixed bytes32 words",
            ));
            continue;
        };
        inputs.push(DockInterfaceArtifactPortInput {
            port: port
                .get("port")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string(),
            stage_identifier: port
                .get("stageIdentifier")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string(),
            hook_name: port
                .get("hookName")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string(),
            hook_id: port
                .get("hookId")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string(),
            canonical_input_signal: port
                .get("canonicalInputSignal")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string(),
            canonical_input_signal_hash: parse_word(port, "canonicalInputSignalHash")
                .unwrap_or([0u8; 32]),
            source: port
                .get("source")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string(),
            source_id,
            signal_id,
            leaf_hash,
        });
    }
    for port in value
        .get("outputs")
        .and_then(Value::as_array)
        .unwrap_or(&Vec::new())
    {
        let port_path = || {
            format!(
                "outputs.{}",
                port.get("port").and_then(Value::as_str).unwrap_or("?")
            )
        };
        let Some(leaf_hash) = parse_word(port, "leafHash") else {
            issues.push(DockIssue::new(
                "D008",
                port_path(),
                "leafHash must be bytes32",
            ));
            continue;
        };
        let (Some(source_id), Some(signal_id)) =
            (parse_word(port, "sourceId"), parse_word(port, "signalId"))
        else {
            issues.push(DockIssue::new(
                "D008",
                port_path(),
                "sourceId/signalId must be 0x-prefixed bytes32 words",
            ));
            continue;
        };
        outputs.push(DockInterfaceArtifactPortOutput {
            port: port
                .get("port")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string(),
            canonical_output_signal: port
                .get("canonicalOutputSignal")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string(),
            canonical_output_signal_hash: parse_word(port, "canonicalOutputSignalHash")
                .unwrap_or([0u8; 32]),
            source: port
                .get("source")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string(),
            source_id,
            signal_id,
            leaf_hash,
        });
    }
    let root_of = |key: &str| -> Option<Word> {
        value
            .get(key)
            .and_then(Value::as_str)
            .and_then(word_from_hex)
    };
    let (Some(inputs_root), Some(outputs_root), Some(interface_leaf)) = (
        root_of("inputsRoot"),
        root_of("outputsRoot"),
        root_of("interfaceRoot"),
    ) else {
        issues.push(DockIssue::new(
            "D008",
            "inputsRoot/outputsRoot/interfaceRoot",
            "must be 0x-prefixed bytes32",
        ));
        return Err(issues);
    };
    if !issues.is_empty() {
        return Err(issues);
    }
    Ok(InterfaceArtifact {
        name,
        order_modes,
        inputs,
        outputs,
        inputs_root,
        outputs_root,
        interface_leaf,
    })
}

// ---------------------------------------------------------------------------
// 已解析 DockRoute v2
// ---------------------------------------------------------------------------

#[derive(Debug, Clone)]
pub struct DockRouteInput {
    pub local_hook_name: String,
    pub target_port: String,
    pub target_input_signal_hash: Word,
    pub target_source_id: Word,
    pub target_signal_id: Word,
    /// Cloud runtime 可读名称：目标 stage 标识与 canonical signal 名
    /// （仅写入 JSON，不参与任何哈希 preimage）。
    pub target_stage_identifier: String,
    pub target_signal_name: String,
    pub binding_hash: Word,
}

#[derive(Debug, Clone)]
pub struct DockRouteOutput {
    pub local_signal_name: String,
    pub local_source_id: Word,
    pub local_signal_id: Word,
    pub target_port: String,
    pub target_output_signal_hash: Word,
    pub target_source_id: Word,
    pub target_signal_id: Word,
    /// Cloud runtime 可读名称（不参与哈希）。
    pub target_signal_name: String,
    pub binding_hash: Word,
}

#[derive(Debug, Clone)]
pub struct DockRoute {
    pub route_id: Word,
    pub local_definition_ref_hash: Word,
    pub stage_identifier: String,
    pub stage_key: Word,
    pub target_definition_ref_hash: Word,
    pub target_zhixu_uid: String,
    pub target_zhixu_name: String,
    pub target_interface_name: String,
    /// `new` | `existing`。
    pub order_mode: String,
    pub target_artifact_hash: Word,
    pub target_cloud_artifact_id: Option<String>,
    pub target_evm_plan_id: Option<Word>,
    /// 被绑定接口的 interfaceLeaf（manifest `interfaces[].interfaceRoot`）。
    pub target_interface_root: Word,
    /// 目标定义级 dockInterfaceRoot（接口叶 merkle root）。
    pub target_dock_interface_root: Word,
    pub source_seam: String,
    pub inputs: Vec<DockRouteInput>,
    pub outputs: Vec<DockRouteOutput>,
    pub inputs_root: Word,
    pub outputs_root: Word,
    pub route_hash: Word,
}

impl DockRoute {
    pub fn to_json(&self) -> Value {
        json!({
            "schemaVersion": DOCK_ROUTE_SCHEMA_VERSION,
            "routeId": word_hex(&self.route_id),
            "local": {
                "definitionRefHash": word_hex(&self.local_definition_ref_hash),
                "stageIdentifier": self.stage_identifier,
                "stageKey": word_hex(&self.stage_key),
            },
            "target": {
                "definitionRefHash": word_hex(&self.target_definition_ref_hash),
                "zhixuUid": self.target_zhixu_uid,
                "zhixuName": self.target_zhixu_name,
                "interfaceName": self.target_interface_name,
                "interfaceRoot": word_hex(&self.target_interface_root),
                "dockInterfaceRoot": word_hex(&self.target_dock_interface_root),
                "artifactHash": word_hex(&self.target_artifact_hash),
                "cloudArtifactId": self.target_cloud_artifact_id.clone(),
                "evmPlanId": self.target_evm_plan_id.map(|word| word_hex(&word)),
            },
            "orderMode": self.order_mode,
            "sourceSeam": self.source_seam,
            "inputBindings": self.inputs.iter().map(|input| json!({
                "localHookName": input.local_hook_name,
                "targetPort": input.target_port,
                "targetInputSignalHash": word_hex(&input.target_input_signal_hash),
                "targetSourceId": word_hex(&input.target_source_id),
                "targetSignalId": word_hex(&input.target_signal_id),
                "targetStageIdentifier": input.target_stage_identifier,
                "targetSignalName": input.target_signal_name,
                "bindingHash": word_hex(&input.binding_hash),
            })).collect::<Vec<_>>(),
            "outputBindings": self.outputs.iter().map(|output| json!({
                "localSignalName": output.local_signal_name,
                "localSourceId": word_hex(&output.local_source_id),
                "localSignalId": word_hex(&output.local_signal_id),
                "targetPort": output.target_port,
                "targetOutputSignalHash": word_hex(&output.target_output_signal_hash),
                "targetSourceId": word_hex(&output.target_source_id),
                "targetSignalId": word_hex(&output.target_signal_id),
                "targetSignalName": output.target_signal_name,
                "bindingHash": word_hex(&output.binding_hash),
            })).collect::<Vec<_>>(),
            "inputBindingsRoot": word_hex(&self.inputs_root),
            "outputBindingsRoot": word_hex(&self.outputs_root),
            "routeHash": word_hex(&self.route_hash),
        })
    }
}

/// 父定义本地身份（link 输入）。
pub struct LocalLinkIdentity {
    pub uid: String,
}

/// Link：本地未链接 routes + resolution manifest → 已解析 DockRoute 列表
/// （D008-D012、D015-D016、D020）。纯函数，无网络、无 I/O。
pub fn link_dock_routes(
    local: &LocalLinkIdentity,
    stages: &[(String, ZhixuStage)],
    unlinked: &[UnlinkedDockRoute],
    manifest: &ResolutionManifest,
) -> DockResult<Vec<DockRoute>> {
    let mut issues = Vec::new();
    let local_definition_ref = definition_ref_hash(&local.uid);
    let stages_by_identifier: BTreeMap<&str, &ZhixuStage> = stages
        .iter()
        .map(|(identifier, stage)| (identifier.as_str(), stage))
        .collect();

    // D008：内容寻址三方一致（entry.zhixu == 内嵌定义派生 uid == route 引用），
    // 且全部叶子/root 由 manifest 数据逐 word 重算——自不一致的 manifest 在
    // link 即拒绝，不推迟到运行期才失败。
    for target in &manifest.targets {
        let entry_path = format!("resolutionManifest.definitions[{}]", target.zhixu);
        let derived_uid = match crate::definition_uid(&target.definition_value) {
            Ok(uid) => uid,
            Err(err) => {
                issues.push(DockIssue::new(
                    "D008",
                    format!("{entry_path}.definition"),
                    format!("cannot derive the definition uid: {err}"),
                ));
                continue;
            }
        };
        if derived_uid != target.zhixu {
            issues.push(DockIssue::new(
                "D008",
                format!("{entry_path}.zhixu"),
                format!(
                    "entry declares {} but the embedded definition derives {} — the manifest is not content-addressed",
                    target.zhixu,
                    display_identity(&target.name, &derived_uid),
                ),
            ));
            continue;
        }
        if definition_ref_hash(&derived_uid) != target.definition_ref_hash {
            issues.push(DockIssue::new(
                "D008",
                format!("{entry_path}.definitionRefHash"),
                "does not match H(UVP_DEFINITION_REF_V1, keccak(uid)) over the embedded definition",
            ));
            continue;
        }
        let uid_word = keccak_word(target.zhixu.as_bytes());
        let mut interface_leaves = Vec::new();
        for interface in &target.interfaces {
            let interface_path = format!("{entry_path}.interfaces.{}", interface.name);
            let Some(modes_word) = order_modes_word(&interface.order_modes) else {
                issues.push(DockIssue::new(
                    "D008",
                    format!("{interface_path}.orderModes"),
                    "orderModes must be a non-empty subset of {new, existing} without duplicates",
                ));
                continue;
            };
            let name_word = interface_name_key(&interface.name);
            let mut input_leaves = Vec::new();
            for port in &interface.inputs {
                let leaf = keccak_words(
                    DOMAIN_INTERFACE_INPUT,
                    &[
                        uid_word,
                        name_word,
                        port_key(&port.port),
                        hook_key(&port.hook_id),
                    ],
                );
                if leaf != port.leaf_hash {
                    issues.push(DockIssue::new(
                        "D008",
                        format!("{interface_path}.inputs.{}", port.port),
                        "leafHash does not match the recomputed input-port preimage",
                    ));
                }
                input_leaves.push(port.leaf_hash);
            }
            let mut output_leaves = Vec::new();
            for port in &interface.outputs {
                let leaf = keccak_words(
                    DOMAIN_INTERFACE_OUTPUT,
                    &[
                        uid_word,
                        name_word,
                        port_key(&port.port),
                        canonical_signal_hash(&port.canonical_output_signal),
                    ],
                );
                if leaf != port.leaf_hash {
                    issues.push(DockIssue::new(
                        "D008",
                        format!("{interface_path}.outputs.{}", port.port),
                        "leafHash does not match the recomputed output-port preimage",
                    ));
                }
                output_leaves.push(port.leaf_hash);
            }
            if merkle_root(&input_leaves) != interface.inputs_root {
                issues.push(DockIssue::new(
                    "D008",
                    format!("{interface_path}.inputsRoot"),
                    "does not match the recomputed root over input-port leaves",
                ));
            }
            if merkle_root(&output_leaves) != interface.outputs_root {
                issues.push(DockIssue::new(
                    "D008",
                    format!("{interface_path}.outputsRoot"),
                    "does not match the recomputed root over output-port leaves",
                ));
            }
            let interface_leaf = keccak_words(
                DOMAIN_INTERFACE,
                &[
                    uid_word,
                    name_word,
                    modes_word,
                    interface.inputs_root,
                    interface.outputs_root,
                ],
            );
            if interface_leaf != interface.interface_leaf {
                issues.push(DockIssue::new(
                    "D008",
                    format!("{interface_path}.interfaceRoot"),
                    "does not match the recomputed interface-leaf preimage",
                ));
            }
            interface_leaves.push(interface.interface_leaf);
        }
        if merkle_root(&interface_leaves) != target.dock_interface_root {
            issues.push(DockIssue::new(
                "D008",
                format!("{entry_path}.interfaces"),
                "the definition-level dockInterfaceRoot does not match the recomputed root over interface leaves",
            ));
        }
    }

    let find_target = |zhixu: &str| -> Option<&ResolutionTarget> {
        manifest.targets.iter().find(|target| target.zhixu == zhixu)
    };

    let mut routes = Vec::new();
    for route in unlinked {
        // issues 按 route 独立收集再合并：若共享一个累积 vec，首个出错
        // route 的残留会让后续 route 在收尾闸处被整体跳过，错误一次报不全。
        let mut route_issues = Vec::new();
        let config = &route.config;
        let path = format!("{}.executor.zhixuExecutorConfig", route.stage_identifier);
        let Some(target_zhixu) = &config.target_zhixu else {
            route_issues.push(DockIssue::new(
                "D008",
                format!("{path}.target"),
                "target is null (dynamic selection): a statically linked compilation cannot resolve this route — cloud runtimes fill dynamic targets from selection records (PRD_100 §10.3)",
            ));
            issues.extend(route_issues);
            continue;
        };
        let Some(target) = find_target(target_zhixu) else {
            route_issues.push(DockIssue::new(
                "D008",
                format!("{path}.target"),
                format!("resolution manifest has no published artifact for {target_zhixu}"),
            ));
            issues.extend(route_issues);
            continue;
        };
        if !target.published || target.artifact_hash == [0u8; 32] {
            route_issues.push(DockIssue::new(
                "D008",
                format!("{path}.target"),
                format!(
                    "target artifact {} is not published/immutable",
                    display_identity(&target.name, &target.zhixu)
                ),
            ));
            issues.extend(route_issues);
            continue;
        }

        // D009：接口按名解析。
        let Some(interface) = target
            .interfaces
            .iter()
            .find(|interface| interface.name == config.interface_name)
        else {
            route_issues.push(DockIssue::new(
                "D009",
                format!("{path}.interface"),
                format!(
                    "target {} has no interface {:?}",
                    display_identity(&target.name, &target.zhixu),
                    config.interface_name
                ),
            ));
            issues.extend(route_issues);
            continue;
        };
        // D020：mode 必须 ∈ 目标接口 orderModes（不静默替代，A05）。
        if !interface
            .order_modes
            .iter()
            .any(|mode| mode == &config.order_mode)
        {
            route_issues.push(DockIssue::new(
                "D020",
                format!("{path}.order.mode"),
                format!(
                    "interface {:?} of target {} allows orderModes {:?}, found {:?}",
                    config.interface_name,
                    display_identity(&target.name, &target.zhixu),
                    interface.order_modes,
                    config.order_mode
                ),
            ));
            issues.extend(route_issues);
            continue;
        }
        let name_word = interface_name_key(&interface.name);
        let route_id_word = route_id(&local_definition_ref, &route.stage_key);

        // D009（端口存在 + 方向）。
        let mut resolved_inputs = Vec::new();
        for (local_hook, port_name) in &config.input_map {
            let Some(port) = interface.inputs.iter().find(|port| &port.port == port_name) else {
                route_issues.push(DockIssue::new(
                    "D009",
                    format!("{path}.inputMap.{local_hook}"),
                    format!(
                        "interface {:?} of target {} has no input port {port_name:?}",
                        config.interface_name, target.zhixu
                    ),
                ));
                continue;
            };
            let hook_key_word = hook_key(&format!("{}#{local_hook}", route.stage_identifier));
            let binding = keccak_words(
                DOMAIN_INPUT_BINDING,
                &[
                    route_id_word,
                    name_word,
                    hook_key_word,
                    port_key(port_name),
                    port.source_id,
                    port.signal_id,
                ],
            );
            resolved_inputs.push(DockRouteInput {
                local_hook_name: local_hook.clone(),
                target_port: port_name.clone(),
                target_input_signal_hash: port.canonical_input_signal_hash,
                target_source_id: port.source_id,
                target_signal_id: port.signal_id,
                target_stage_identifier: port.stage_identifier.clone(),
                target_signal_name: port.canonical_input_signal.clone(),
                binding_hash: binding,
            });
        }

        let mut resolved_outputs = Vec::new();
        let stage = stages_by_identifier
            .get(route.stage_identifier.as_str())
            .copied()
            .expect("unlinked route stage exists");
        for (local_signal, port_name) in &config.signal_map {
            let Some(port) = interface
                .outputs
                .iter()
                .find(|port| &port.port == port_name)
            else {
                route_issues.push(DockIssue::new(
                    "D009",
                    format!("{path}.signalMap.{local_signal}"),
                    format!(
                        "interface {:?} of target {} has no output port {port_name:?}",
                        config.interface_name, target.zhixu
                    ),
                ));
                continue;
            };
            let local_source_id = keccak_word(stage.source.as_bytes());
            let local_signal_id =
                keccak_word(format!("{}.{}", route.stage_identifier, local_signal).as_bytes());
            let binding = keccak_words(
                DOMAIN_OUTPUT_BINDING,
                &[
                    route_id_word,
                    name_word,
                    local_source_id,
                    local_signal_id,
                    port_key(port_name),
                    port.source_id,
                    port.signal_id,
                ],
            );
            resolved_outputs.push(DockRouteOutput {
                local_signal_name: local_signal.clone(),
                local_source_id,
                local_signal_id,
                target_port: port_name.clone(),
                target_output_signal_hash: port.canonical_output_signal_hash,
                target_source_id: port.source_id,
                target_signal_id: port.signal_id,
                target_signal_name: port.canonical_output_signal.clone(),
                binding_hash: binding,
            });
        }

        // D012：被绑定端口同一 source seam。
        let mut seams = BTreeSet::new();
        for input in &resolved_inputs {
            if let Some(port) = interface
                .inputs
                .iter()
                .find(|port| port.port == input.target_port)
            {
                seams.insert(port.source.clone());
            }
        }
        for output in &resolved_outputs {
            if let Some(port) = interface
                .outputs
                .iter()
                .find(|port| port.port == output.target_port)
            {
                seams.insert(port.source.clone());
            }
        }
        if seams.len() != 1 {
            route_issues.push(DockIssue::new(
                "D012",
                &path,
                format!(
                    "all ports bound by one route must share a single target source seam, found {seams:?}"
                ),
            ));
            issues.extend(route_issues);
            continue;
        }
        let source_seam = seams.iter().next().cloned().unwrap_or_default();

        // D016：binding 数量上限。
        if resolved_inputs.len() > MAX_DOCK_INPUTS {
            route_issues.push(DockIssue::new(
                "D016",
                &path,
                format!(
                    "route references {} input bindings, limit is {MAX_DOCK_INPUTS}",
                    resolved_inputs.len()
                ),
            ));
        }
        if resolved_outputs.len() > MAX_DOCK_OUTPUTS {
            route_issues.push(DockIssue::new(
                "D016",
                &path,
                format!(
                    "route references {} output bindings, limit is {MAX_DOCK_OUTPUTS}",
                    resolved_outputs.len()
                ),
            ));
        }
        // 收尾闸只看本 route 的 issues：route_issues 为空则照常产 route，
        // 前序 route 的失败不得让后续干净 route 的哈希/根计算被跳过。
        if !route_issues.is_empty() {
            issues.append(&mut route_issues);
            continue;
        }

        resolved_inputs.sort_by_key(|input| input.binding_hash);
        resolved_outputs.sort_by_key(|output| output.binding_hash);
        let inputs_root = merkle_root(
            &resolved_inputs
                .iter()
                .map(|input| input.binding_hash)
                .collect::<Vec<_>>(),
        );
        let outputs_root = merkle_root(
            &resolved_outputs
                .iter()
                .map(|output| output.binding_hash)
                .collect::<Vec<_>>(),
        );
        let mode_word_value = mode_word(&config.order_mode).expect("mode validated (D004)");
        let route_hash = keccak_words(
            DOMAIN_ROUTE,
            &[
                local_definition_ref,
                target.definition_ref_hash,
                name_word,
                mode_word_value,
                inputs_root,
                outputs_root,
            ],
        );
        routes.push(DockRoute {
            route_id: route_id_word,
            local_definition_ref_hash: local_definition_ref,
            stage_identifier: route.stage_identifier.clone(),
            stage_key: route.stage_key,
            target_definition_ref_hash: target.definition_ref_hash,
            target_zhixu_uid: target.zhixu.clone(),
            target_zhixu_name: target.name.clone(),
            target_interface_name: interface.name.clone(),
            order_mode: config.order_mode.clone(),
            target_artifact_hash: target.artifact_hash,
            target_cloud_artifact_id: target.cloud_artifact_id.clone(),
            target_evm_plan_id: target.evm_plan_id,
            target_interface_root: interface.interface_leaf,
            target_dock_interface_root: target.dock_interface_root,
            source_seam,
            inputs: resolved_inputs,
            outputs: resolved_outputs,
            inputs_root,
            outputs_root,
            route_hash,
        });
    }

    if !issues.is_empty() {
        return Err(issues);
    }

    // D015：route 启动图无环且深度受限。节点为 zhixu 派生 uid（内容寻址
    // 身份），边为 resolved route 与 manifest 提供的目标自身 dockEdges。
    let mut edges: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
    let local_node = local.uid.clone();
    for target in &manifest.targets {
        let node = target.zhixu.clone();
        for zhixu in &target.dock_edges {
            edges.entry(node.clone()).or_default().insert(zhixu.clone());
        }
    }
    for route in &routes {
        edges
            .entry(local_node.clone())
            .or_default()
            .insert(route.target_zhixu_uid.clone());
    }
    if let Some(cycle) = find_route_cycle(&edges) {
        issues.push(DockIssue::new(
            "D015",
            "dockRoutes",
            format!(
                "dock route startup graph has a reachable cycle: {}",
                cycle.join(" -> ")
            ),
        ));
    } else {
        let depth = max_reachable_route_depth(&edges, &local_node);
        if depth > usize::from(MAX_DOCK_DEPTH) {
            issues.push(DockIssue::new(
                "D015",
                "dockRoutes",
                format!(
                    "dock route startup graph depth {depth} exceeds MAX_DOCK_DEPTH {MAX_DOCK_DEPTH}"
                ),
            ));
        }
    }

    if !issues.is_empty() {
        return Err(issues);
    }
    routes.sort_by_key(|route| route.route_id);
    Ok(routes)
}

fn find_route_cycle(edges: &BTreeMap<String, BTreeSet<String>>) -> Option<Vec<String>> {
    for start in edges.keys() {
        let mut parents: BTreeMap<String, String> = BTreeMap::new();
        let mut queue: std::collections::VecDeque<String> = std::collections::VecDeque::new();
        for next in edges.get(start).into_iter().flatten() {
            if next == start {
                return Some(vec![start.clone(), start.clone()]);
            }
            if parents.insert(next.clone(), start.clone()).is_none() {
                queue.push_back(next.clone());
            }
        }
        while let Some(current) = queue.pop_front() {
            for next in edges.get(&current).into_iter().flatten() {
                if next == start {
                    let mut cycle = vec![current.clone()];
                    let mut node = current.clone();
                    while node != *start {
                        node = parents[&node].clone();
                        cycle.push(node.clone());
                    }
                    cycle.reverse();
                    cycle.push(start.clone());
                    return Some(cycle);
                }
                if parents.insert(next.clone(), current.clone()).is_none() {
                    queue.push_back(next.clone());
                }
            }
        }
    }
    None
}

/// Return the maximum number of nodes on a route-startup path rooted at the
/// local definition. Cycles are rejected by `find_route_cycle` before this is
/// called, so the longest-path calculation is finite and deterministic.
fn max_reachable_route_depth(edges: &BTreeMap<String, BTreeSet<String>>, root: &str) -> usize {
    let mut depths = BTreeMap::from([(root.to_string(), 1usize)]);
    let mut queue = VecDeque::from([(root.to_string(), 1usize)]);
    let mut maximum = 1usize;
    while let Some((node, depth)) = queue.pop_front() {
        maximum = maximum.max(depth);
        for next in edges.get(&node).into_iter().flatten() {
            let next_depth = depth + 1;
            let should_visit = depths
                .get(next)
                .is_none_or(|known_depth| next_depth > *known_depth);
            if should_visit {
                depths.insert(next.clone(), next_depth);
                queue.push_back((next.clone(), next_depth));
            }
        }
    }
    maximum
}

/// `dockRoutesRoot`：一个定义全部已解析 route 的 routeHash Merkle root。
pub fn dock_routes_root(routes: &[DockRoute]) -> Word {
    merkle_root(
        &routes
            .iter()
            .map(|route| route.route_hash)
            .collect::<Vec<_>>(),
    )
}

// ---------------------------------------------------------------------------
// EIP-712 entrance permit digest（golden vector 与 TS/Solidity 对齐用）
// ---------------------------------------------------------------------------

pub const PERMIT_TYPEHASH_SUFFIX: &str = "UVPDockEntrancePermitV2(bytes32 targetPlanId,bytes32 targetEntrancePortId,bytes32 interfaceNameId,bytes32 localPlanId,bytes32 routeHash,bytes32 dockInstanceId,bytes32 linkedOrderId,uint256 feeLimit,uint256 nonce,uint256 deadline)";

/// 域 version 由调用方传入（链侧随 docking module abiVersion 演进）。
pub fn eip712_permit_domain_separator(
    chain_id: u64,
    verifying_contract: &str,
    version: &str,
) -> Option<Word> {
    let address = address_word(verifying_contract)?;
    Some(keccak_words(
        "EIP712Domain(string name,string version,uint256 chainId,address verifyingContract)",
        &[
            keccak_word(b"UVPDockingModule"),
            keccak_word(version.as_bytes()),
            u64_word(chain_id),
            address,
        ],
    ))
}

/// 注意：EIP-712 structHash/typehash 与 domain 的编码是
/// `keccak256(concat(...))`（无 domain word 前缀），与 keccak_words 不同；
/// 这里按 EIP-712 规范逐字实现，Solidity 端用 abi.encode 得到相同结果。
#[allow(clippy::too_many_arguments)]
pub fn eip712_permit_struct_hash(
    target_plan_id: &Word,
    target_entrance_port_id: &Word,
    interface_name_id: &Word,
    local_plan_id: &Word,
    route_hash: &Word,
    dock_instance: &Word,
    linked_order: &Word,
    nonce: u64,
    deadline: u64,
) -> Option<Word> {
    let fee_limit = 0u64; // 无费用机制，feeLimit 固定 0（与合约一致）
    let mut buf = Vec::with_capacity(32 * 10);
    buf.extend_from_slice(&keccak_word(PERMIT_TYPEHASH_SUFFIX.as_bytes()));
    for word in [
        target_plan_id,
        target_entrance_port_id,
        interface_name_id,
        local_plan_id,
        route_hash,
        dock_instance,
        linked_order,
    ] {
        buf.extend_from_slice(word);
    }
    buf.extend_from_slice(&u64_word(fee_limit));
    buf.extend_from_slice(&u64_word(nonce));
    buf.extend_from_slice(&u64_word(deadline));
    Some(keccak_word(&buf))
}

#[allow(clippy::too_many_arguments)]
pub fn eip712_permit_digest(
    chain_id: u64,
    verifying_contract: &str,
    version: &str,
    target_plan_id: &Word,
    target_entrance_port_id: &Word,
    interface_name_id: &Word,
    local_plan_id: &Word,
    route_hash: &Word,
    dock_instance: &Word,
    linked_order: &Word,
    nonce: u64,
    deadline: u64,
) -> Option<Word> {
    let domain_separator = eip712_permit_domain_separator(chain_id, verifying_contract, version)?;
    let struct_hash = eip712_permit_struct_hash(
        target_plan_id,
        target_entrance_port_id,
        interface_name_id,
        local_plan_id,
        route_hash,
        dock_instance,
        linked_order,
        nonce,
        deadline,
    )?;
    let mut buf = Vec::with_capacity(2 + 64);
    buf.extend_from_slice(b"\x19\x01");
    buf.extend_from_slice(&domain_separator);
    buf.extend_from_slice(&struct_hash);
    Some(keccak_word(&buf))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mode_and_order_modes_words_are_pinned() {
        assert_eq!(mode_word("new").unwrap(), u8_word(0));
        assert_eq!(mode_word("existing").unwrap(), u8_word(1));
        assert!(mode_word("derived-v1").is_none());
        assert_eq!(
            order_modes_word(&["new".to_string()]).unwrap(),
            u8_word(0b01)
        );
        assert_eq!(
            order_modes_word(&["existing".to_string()]).unwrap(),
            u8_word(0b10)
        );
        assert_eq!(
            order_modes_word(&["existing".to_string(), "new".to_string()]).unwrap(),
            u8_word(0b11)
        );
        assert!(order_modes_word(&[]).is_none());
        assert!(order_modes_word(&["new".to_string(), "new".to_string()]).is_none());
        assert!(order_modes_word(&["bogus".to_string()]).is_none());
    }

    #[test]
    fn derived_uid_shape_is_enforced_on_targets() {
        assert!(valid_derived_uid("zx-0123456789abcdef0123456789abcdef"));
        assert!(!valid_derived_uid("zx-payment-execution"));
        assert!(!valid_derived_uid("0123456789abcdef0123456789abcdef"));
        assert!(!valid_derived_uid("zx-0123456789ABCDEF0123456789ABCDEF"));
        assert!(!valid_derived_uid("zx-0123456789abcdef0123456789abcde"));
    }

    #[test]
    fn dock_instance_id_separates_modes_and_order_refs() {
        let domain = [1u8; 32];
        let plan = [2u8; 32];
        let def_ref = [3u8; 32];
        let order = [4u8; 32];
        let route_id = [5u8; 32];
        let route_hash = [6u8; 32];
        let new_mode = mode_word("new").unwrap();
        let existing_mode = mode_word("existing").unwrap();
        let name = interface_name_key("production_service");
        let order_ref = target_order_ref_key("factory-a/P001");
        let new_instance = dock_instance_id(
            &domain,
            &plan,
            &def_ref,
            &order,
            &route_id,
            &route_hash,
            &new_mode,
            &name,
            None,
        );
        let existing_same_ref = dock_instance_id(
            &domain,
            &plan,
            &def_ref,
            &order,
            &route_id,
            &route_hash,
            &existing_mode,
            &name,
            Some(&order_ref),
        );
        let existing_other_ref = dock_instance_id(
            &domain,
            &plan,
            &def_ref,
            &order,
            &route_id,
            &route_hash,
            &existing_mode,
            &name,
            Some(&target_order_ref_key("factory-a/P002")),
        );
        assert_ne!(new_instance, existing_same_ref);
        assert_ne!(existing_same_ref, existing_other_ref);
    }

    #[test]
    fn manifest_output_ports_with_missing_identity_words_are_rejected() {
        // output 端口 sourceId/signalId 缺失/非法不得静默落成零 word——
        // 与 input 端口同口径的 D008 确定性错误。
        let interface = json!({
            "name": "production_evidence",
            "orderModes": ["existing"],
            "inputs": [],
            "outputs": [{
                "port": "scrap_declared",
                "canonicalOutputSignal": "factory::manufacturing.produce.scrap_created",
                "canonicalOutputSignalHash": word_hex(&[0xaa; 32]),
                "source": "factory",
                "sourceId": word_hex(&[0xab; 32]),
                "signalId": word_hex(&[0xbb; 32]),
                "leafHash": word_hex(&[0xcc; 32])
            }],
            "inputsRoot": word_hex(&EMPTY_MERKLE_ROOT),
            "outputsRoot": word_hex(&[0x01; 32]),
            "interfaceRoot": word_hex(&[0xdd; 32])
        });
        let manifest = |interface: Value| {
            json!({
                "schemaVersion": DOCK_RESOLUTION_SCHEMA_VERSION,
                "definitions": [{
                    "zhixu": "zx-0123456789abcdef0123456789abcdef",
                    "definition": { "apiVersion": "uvp/v0" },
                    "definitionRefHash": word_hex(&[0xee; 32]),
                    "artifactHash": word_hex(&[0xef; 32]),
                    "published": true,
                    "interfaces": [interface]
                }]
            })
        };
        for (field, removal) in [("sourceId", true), ("signalId", false)] {
            let mut poisoned = interface.clone();
            if removal {
                poisoned["outputs"][0]
                    .as_object_mut()
                    .unwrap()
                    .remove(field);
            } else {
                poisoned["outputs"][0][field] = json!("not-a-word");
            }
            let issues = parse_resolution_manifest(&manifest(poisoned)).unwrap_err();
            assert!(
                issues.iter().any(|issue| {
                    issue.message.contains(field) && issue.message.contains("must be")
                }),
                "{field}: {issues:?}"
            );
        }
    }

    #[test]
    fn route_depth_counts_local_definition_and_targets() {
        let root = "zx-0";
        let mut edges = BTreeMap::new();
        for index in 0..7 {
            edges
                .entry(if index == 0 {
                    root.to_string()
                } else {
                    format!("target-{index}")
                })
                .or_insert_with(BTreeSet::new)
                .insert(format!("target-{}", index + 1));
        }
        assert_eq!(max_reachable_route_depth(&edges, root), 8);

        edges
            .entry("target-7".to_string())
            .or_insert_with(BTreeSet::new)
            .insert("target-8".to_string());
        assert_eq!(max_reachable_route_depth(&edges, root), 9);
        assert!(9 > usize::from(MAX_DOCK_DEPTH));
    }

    #[test]
    fn route_depth_ignores_disconnected_manifest_edges() {
        let mut edges = BTreeMap::new();
        edges.insert(
            "unrelated".to_string(),
            BTreeSet::from(["unrelated-child".to_string()]),
        );
        assert_eq!(max_reachable_route_depth(&edges, "local"), 1);
    }
}

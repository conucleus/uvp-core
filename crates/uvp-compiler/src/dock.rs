//! Zhixu Dock 统一委托协议（PRD93-96）的语义权威实现。
//!
//! 本模块固定：
//! - `uvp.dock.v1` source schema（目标 `spec.dockInterface` + 调用方
//!   `executor.zhixuExecutorConfig`）；
//! - 跨定义 linker（resolution manifest 输入，纯函数，无网络）；
//! - 全部跨运行时 commitment 的 keccak/ABI-word 编码、Merkle root、
//!   `dockInstanceId`/`linkedOrderId` 推导与 envelope 幂等键；
//! - 编译期错误码 D001-D020（另加接口形状 D021-D026）。
//!
//! 哈希规则（M0 冻结，Rust/TS/Solidity/Go 必须逐字节一致）：
//! - 所有 commitment 哈希 = `keccak256(domainWord ‖ w1 ‖ … ‖ wn)`，其中
//!   `domainWord = keccak256("<DOMAIN>")`，`wi` 为 32-byte word。这与
//!   Solidity `keccak256(abi.encode(keccak256("<DOMAIN>"), …))` 完全一致
//!   （PRD94 §7.2：全部字段均为 word，无动态类型，禁 encodePacked）。
//! - Merkle：叶子为 word；空集合 root = `keccak256("")`
//!   （`EMPTY_MERKLE_ROOT`）；配对合并 `keccak256(min(a,b) ‖ max(a,b))`
//!   （字节序取小者为左）；叶子列表先按字节升序排序再建树。
//! - 枚举 word：input kind 0=signal/1=entrance；access 0=open/1=permit/
//!   2=linked；terminal 0=none/1=success/2=failure/3=cancelled；
//!   idPolicy 0=derived-v1。

use serde_json::{json, Map, Value};
use sha3::{Digest, Keccak256};
use std::collections::{BTreeMap, BTreeSet};
use uvp_hook_dsl::{parse_hook, DependencyKind, HookMode, ParseHookRequest, Profile};
use uvp_model::{DockInterfaceSource, ZhixuStage};

pub type Word = [u8; 32];

// ---------------------------------------------------------------------------
// M0 冻结常量
// ---------------------------------------------------------------------------

pub const DOCK_SCHEMA_VERSION: &str = "uvp.dock.v1";
pub const DOCK_INTERFACE_ARTIFACT_SCHEMA_VERSION: &str = "uvp.dockInterfaceArtifact.v1";
pub const DOCK_ROUTE_SCHEMA_VERSION: &str = "uvp.dockRoute.v1";
pub const DOCK_RESOLUTION_SCHEMA_VERSION: &str = "uvp.dock.resolution.v1";
pub const DOCK_COMPAT_SCHEMA_VERSION: &str = "uvp.dock.compat.v1";

pub const MAX_DOCK_INPUTS: usize = 8;
pub const MAX_DOCK_OUTPUTS: usize = 16;
pub const MAX_DOCK_DEPTH: u8 = 8;
/// `^[a-z][a-z0-9_]{0,31}$`（PRD94 §3.2）。
pub const MAX_PORT_NAME_BYTES: usize = 32;

pub const DOMAIN_DEFINITION_REF: &str = "UVP_DEFINITION_REF_V1";
pub const DOMAIN_INTERFACE_INPUT: &str = "UVP_DOCK_INTERFACE_INPUT_V1";
pub const DOMAIN_INTERFACE_OUTPUT: &str = "UVP_DOCK_INTERFACE_OUTPUT_V1";
pub const DOMAIN_ROUTE_ID: &str = "UVP_DOCK_ROUTE_ID_V1";
pub const DOMAIN_INPUT_BINDING: &str = "UVP_DOCK_INPUT_BINDING_V1";
pub const DOMAIN_OUTPUT_BINDING: &str = "UVP_DOCK_OUTPUT_BINDING_V1";
pub const DOMAIN_ROUTE: &str = "UVP_DOCK_ROUTE_V1";
pub const DOMAIN_DOCK_INSTANCE: &str = "UVP_DOCK_INSTANCE_V1";
pub const DOMAIN_DOCK_ORDER: &str = "UVP_DOCK_ORDER_V1";
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

fn enum_word(value: &str, table: &[&str]) -> Option<Word> {
    table
        .iter()
        .position(|candidate| candidate == &value)
        .map(|position| u8_word(position as u8))
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
// 身份推导（PRD94 §7.3-§7.5）
// ---------------------------------------------------------------------------

/// `definitionRefHash = H("UVP_DEFINITION_REF_V1", keccak(uid), keccak(version))`
pub fn definition_ref_hash(uid: &str, version: &str) -> Word {
    keccak_words(
        DOMAIN_DEFINITION_REF,
        &[keccak_word(uid.as_bytes()), keccak_word(version.as_bytes())],
    )
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

pub fn canonical_signal_hash(canonical: &str) -> Word {
    keccak_word(canonical.as_bytes())
}

/// StateMachine 事实寻址键：`keccak256(abi.encode(sourceId, signalId))`
/// （64 字节拼接，无 domain）。output 幂等键的 targetFactId 在链上只能
/// 从 word 推导，采用本键（PRD95 §9）。
pub fn signal_key(source_id: &Word, signal_id: &Word) -> Word {
    let mut buf = [0u8; 64];
    buf[..32].copy_from_slice(source_id);
    buf[32..].copy_from_slice(signal_id);
    keccak_word(&buf)
}

pub fn route_id(local_definition_ref: &Word, stage_key_word: &Word) -> Word {
    keccak_words(DOMAIN_ROUTE_ID, &[*local_definition_ref, *stage_key_word])
}

/// EVM runtime domain：chainId + StateMachine 地址（PRD94 §7.5）。
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

/// 字符串 orderID 先哈希成 word（v1 入口 API 兼容路径）。
pub fn local_order_key(order_id: &str) -> Word {
    keccak_word(order_id.as_bytes())
}

pub fn dock_instance_id(
    runtime_domain: &Word,
    local_definition_ref: &Word,
    local_order_key: &Word,
    route_id_word: &Word,
    route_hash: &Word,
) -> Word {
    keccak_words(
        DOMAIN_DOCK_INSTANCE,
        &[
            *runtime_domain,
            *local_definition_ref,
            *local_order_key,
            *route_id_word,
            *route_hash,
        ],
    )
}

pub fn linked_order_id(dock_instance_id: &Word, target_definition_ref: &Word) -> Word {
    keccak_words(
        DOMAIN_DOCK_ORDER,
        &[*dock_instance_id, *target_definition_ref],
    )
}

/// Dock input envelope payload hash（PRD95 §3.1）。全部字段 word 化；
/// target 信号词使用 StateMachine 事实寻址的 signalId（keccak(task.stage.signal)），
/// 与 UVPDockingModule._inputPayloadHash 逐字一致。
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
    source_fact_set_hash: &Word,
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
            *source_fact_set_hash,
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

const MIGRATION_HINT: &str = "legacy Zhixu delegation was removed (clean break, PRD94 §13): \
    publish target spec.dockInterface ports and bind executor.zhixuExecutorConfig \
    {schemaVersion:uvp.dock.v1, target, order.idPolicy:derived-v1, inputMap, signalMap-to-port-names}; \
    re-link and republish, do not expect runtime compatibility";

// ---------------------------------------------------------------------------
// 调用方 executor config（source 层）
// ---------------------------------------------------------------------------

#[derive(Debug, Clone)]
pub struct ZhixuExecutorConfigV1 {
    pub target_zhixu: String,
    pub target_version: String,
    pub input_map: BTreeMap<String, String>,
    pub signal_map: BTreeMap<String, String>,
}

/// 解析并本地校验 `executor.zhixuExecutorConfig`（D001-D007）。
/// `stage` 为该 executor 所属 stage；`path` 为报错 JSON path 前缀。
pub fn parse_zhixu_executor_config(
    executor_value: &Value,
    stage: &ZhixuStage,
    stage_identifier: &str,
) -> DockResult<ZhixuExecutorConfigV1> {
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

    // D002：schemaVersion + 未知/旧字段（迁移硬错误）。
    let schema = config_object
        .get("schemaVersion")
        .and_then(Value::as_str)
        .unwrap_or_default();
    if schema != DOCK_SCHEMA_VERSION {
        issues.push(DockIssue::new(
            "D002",
            format!("{path}.schemaVersion"),
            format!("must be \"{DOCK_SCHEMA_VERSION}\", found {schema:?}"),
        ));
    }
    const ALLOWED_KEYS: [&str; 5] = ["schemaVersion", "target", "order", "inputMap", "signalMap"];
    for key in config_object.keys() {
        if !ALLOWED_KEYS.contains(&key.as_str()) {
            let message = if key == "triggerEntrance" {
                format!("unknown field {key:?}: {MIGRATION_HINT}")
            } else {
                format!("unknown field {key:?}; allowed: {ALLOWED_KEYS:?}")
            };
            issues.push(DockIssue::new("D002", format!("{path}.{key}"), message));
        }
    }

    let target_zhixu = config_object
        .get("target")
        .and_then(|target| target.get("zhixu"))
        .and_then(Value::as_str)
        .unwrap_or_default()
        .trim()
        .to_string();
    let target_version = config_object
        .get("target")
        .and_then(|target| target.get("version"))
        .and_then(Value::as_str)
        .unwrap_or_default()
        .trim()
        .to_string();
    // D003：精确版本；禁止 latest/范围/空白。
    if target_zhixu.is_empty() {
        issues.push(DockIssue::new(
            "D003",
            format!("{path}.target.zhixu"),
            "target zhixu UID is required (immutable catalog UID, not display name)",
        ));
    }
    if !is_exact_version(&target_version) {
        issues.push(DockIssue::new(
            "D003",
            format!("{path}.target.version"),
            format!("target version must be an exact published immutable version, found {target_version:?}"),
        ));
    }

    // D004：order.idPolicy。
    let id_policy = config_object
        .get("order")
        .and_then(|order| order.get("idPolicy"))
        .and_then(Value::as_str)
        .unwrap_or_default();
    if id_policy != "derived-v1" {
        issues.push(DockIssue::new(
            "D004",
            format!("{path}.order.idPolicy"),
            format!("must be \"derived-v1\", found {id_policy:?}"),
        ));
    }

    let empty_map = Map::new();
    let input_map = config_object
        .get("inputMap")
        .and_then(Value::as_object)
        .unwrap_or(&empty_map);
    let signal_map = config_object
        .get("signalMap")
        .and_then(Value::as_object)
        .unwrap_or(&empty_map);
    if config_object.get("inputMap").is_none() || input_map.is_empty() {
        issues.push(DockIssue::new(
            "D005",
            format!("{path}.inputMap"),
            "inputMap is required and must bind the entrance input port",
        ));
    }
    if config_object.get("signalMap").is_none() {
        issues.push(DockIssue::new(
            "D006",
            format!("{path}.signalMap"),
            "signalMap is required",
        ));
    }

    // D005：inputMap key 必须是本地 receive hook；value 必须是合法端口名。
    let mut parsed_input = BTreeMap::new();
    for (hook_name, port) in input_map {
        if !stage.receive_signals.contains_key(hook_name) {
            issues.push(DockIssue::new(
                "D005",
                format!("{path}.inputMap.{hook_name}"),
                format!("key is not a receiveSignals hook of stage {stage_identifier}"),
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
                format!("value must be a port name matching ^[a-z][a-z0-9_]{{0,31}}$, found {port_name:?}; {MIGRATION_HINT}"),
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

    // D006/D007：signalMap key 必须是本地 send signal；必须含 str/cmp。
    let mut parsed_signal = BTreeMap::new();
    for (signal_name, port) in signal_map {
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
                format!("value must be a port name matching ^[a-z][a-z0-9_]{{0,31}}$, found {port_name:?}; {MIGRATION_HINT}"),
            ));
            continue;
        }
        parsed_signal.insert(signal_name.clone(), port_name.to_string());
    }
    if !parsed_signal.contains_key("str") || !parsed_signal.contains_key("cmp") {
        issues.push(DockIssue::new(
            "D007",
            &path,
            "signalMap must contain at least the local str and cmp signals",
        ));
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

    if !issues.is_empty() {
        return Err(issues);
    }
    Ok(ZhixuExecutorConfigV1 {
        target_zhixu,
        target_version,
        input_map: parsed_input,
        signal_map: parsed_signal,
    })
}

fn is_exact_version(version: &str) -> bool {
    !version.is_empty()
        && version == version.trim()
        && version != "latest"
        && !version
            .chars()
            .any(|ch| matches!(ch, '^' | '~' | '>' | '<' | '=' | '*' | ' '))
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
    pub config: ZhixuExecutorConfigV1,
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

// ---------------------------------------------------------------------------
// 目标接口（dockInterface → DockInterfaceArtifactV1）
// ---------------------------------------------------------------------------

#[derive(Debug, Clone)]
pub struct DockInterfaceArtifactPortInput {
    pub port: String,
    pub kind: String, // entrance | signal
    pub stage_identifier: String,
    pub hook_name: String,
    pub hook_id: String,
    pub canonical_input_signal: String,
    pub canonical_input_signal_hash: Word,
    pub source: String,
    pub source_id: Word,
    pub signal_id: Word,
    pub access_policy: String, // open | permit | linked
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
    pub terminal: String, // none | success | failure | cancelled
    pub leaf_hash: Word,
}

#[derive(Debug, Clone)]
pub struct DockInterfaceArtifact {
    pub uid: String,
    pub version: String,
    pub definition_ref_hash: Word,
    pub inputs: Vec<DockInterfaceArtifactPortInput>,
    pub outputs: Vec<DockInterfaceArtifactPortOutput>,
    pub interface_root: Word,
}

const INPUT_KIND_TABLE: [&str; 2] = ["signal", "entrance"];
const ACCESS_TABLE: [&str; 3] = ["open", "permit", "linked"];
const TERMINAL_TABLE: [&str; 4] = ["none", "success", "failure", "cancelled"];

/// 编译目标定义的 `spec.dockInterface`（D013/D014/D019 + D021-D026）。
pub fn compile_dock_interface(
    dock: &DockInterfaceSource,
    uid: &str,
    version: &str,
    entries: &[(String, ZhixuStage)],
) -> DockResult<DockInterfaceArtifact> {
    let mut issues = Vec::new();
    if dock.schema_version != DOCK_SCHEMA_VERSION {
        issues.push(DockIssue::new(
            "D002",
            "spec.dockInterface.schemaVersion",
            format!("must be \"{DOCK_SCHEMA_VERSION}\""),
        ));
    }

    let stages_by_identifier: BTreeMap<&str, &ZhixuStage> = entries
        .iter()
        .map(|(identifier, stage)| (identifier.as_str(), stage))
        .collect();
    let mut hooks_claimed: BTreeMap<String, String> = BTreeMap::new();
    let mut inputs = Vec::new();
    let mut outputs = Vec::new();
    let mut leaves = Vec::new();

    for (port_name, port) in &dock.inputs {
        let path = format!("spec.dockInterface.inputs.{port_name}");
        if !valid_port_name(port_name) {
            issues.push(DockIssue::new(
                "D021",
                &path,
                "port name must match ^[a-z][a-z0-9_]{0,31}$",
            ));
            continue;
        }
        if !INPUT_KIND_TABLE.contains(&port.kind.as_str()) {
            issues.push(DockIssue::new(
                "D021",
                format!("{path}.kind"),
                format!("kind must be entrance or signal, found {:?}", port.kind),
            ));
            continue;
        }
        // D023：access policy 与 kind 匹配。
        let policy = port.access.policy.as_str();
        let valid_policy = if port.kind == "entrance" {
            matches!(policy, "open" | "permit")
        } else {
            policy == "linked"
        };
        if !valid_policy {
            issues.push(DockIssue::new(
                "D023",
                format!("{path}.access.policy"),
                format!(
                    "entrance ports allow open|permit, signal ports are fixed to linked; found {policy:?}"
                ),
            ));
            continue;
        }

        // D022/D013：hook 引用 + 单一正向本域 atom。
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
        hooks_claimed.insert(port.hook.clone(), port_name.clone());

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
        let port_key_word = port_key(port_name);
        let kind_word = enum_word(&port.kind, &INPUT_KIND_TABLE).expect("kind validated");
        let access_word = enum_word(policy, &ACCESS_TABLE).expect("policy validated");
        let hook_key_word = hook_key(&port.hook);
        // 叶子提交合约可验证的 word：sourceId/signalId（与 StateMachine 的
        // 事实寻址键一致），而非合约无法分解的 canonical 字符串哈希。
        let source_id_word = keccak_word(stage.source.as_bytes());
        let signal_id_word = keccak_word(dependency.signal_name.as_bytes());
        let leaf_hash = keccak_words(
            DOMAIN_INTERFACE_INPUT,
            &[
                definition_ref_hash(uid, version),
                port_key_word,
                kind_word,
                hook_key_word,
                source_id_word,
                signal_id_word,
                access_word,
            ],
        );
        leaves.push(leaf_hash);
        inputs.push(DockInterfaceArtifactPortInput {
            port: port_name.clone(),
            kind: port.kind.clone(),
            stage_identifier: stage_identifier.clone(),
            hook_name: hook_name.clone(),
            hook_id: port.hook.clone(),
            canonical_input_signal,
            canonical_input_signal_hash: canonical_hash,
            source: stage.source.clone(),
            source_id: source_id_word,
            signal_id: signal_id_word,
            access_policy: policy.to_string(),
            leaf_hash,
        });
    }

    for (port_name, port) in &dock.outputs {
        let path = format!("spec.dockInterface.outputs.{port_name}");
        if !valid_port_name(port_name) {
            issues.push(DockIssue::new(
                "D021",
                &path,
                "port name must match ^[a-z][a-z0-9_]{0,31}$",
            ));
            continue;
        }
        let terminal = port.terminal.clone().unwrap_or_else(|| "none".to_string());
        if !TERMINAL_TABLE.contains(&terminal.as_str()) {
            issues.push(DockIssue::new(
                "D024",
                format!("{path}.terminal"),
                format!(
                    "terminal must be one of success|failure|cancelled (or omitted), found {terminal:?}"
                ),
            ));
            continue;
        }
        // D014：真实 send capability。
        let Some((source, stage_identifier, signal_name)) = parse_canonical_signal(&port.signal)
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
        let terminal_word = enum_word(&terminal, &TERMINAL_TABLE).expect("terminal validated");
        let source_id_word = keccak_word(source.as_bytes());
        let signal_id_word = keccak_word(format!("{stage_identifier}.{signal_name}").as_bytes());
        let leaf_hash = keccak_words(
            DOMAIN_INTERFACE_OUTPUT,
            &[
                definition_ref_hash(uid, version),
                port_key(port_name),
                source_id_word,
                signal_id_word,
                terminal_word,
            ],
        );
        leaves.push(leaf_hash);
        outputs.push(DockInterfaceArtifactPortOutput {
            port: port_name.clone(),
            canonical_output_signal: port.signal.clone(),
            canonical_output_signal_hash: canonical_hash,
            source: source.to_string(),
            source_id: source_id_word,
            signal_id: signal_id_word,
            terminal,
            leaf_hash,
        });
    }

    if !issues.is_empty() {
        return Err(issues);
    }
    // 数组按端口名 UTF-8 字节升序（PRD94 §6）。
    inputs.sort_by_key(|port| port.port.clone());
    outputs.sort_by_key(|port| port.port.clone());
    Ok(DockInterfaceArtifact {
        uid: uid.to_string(),
        version: version.to_string(),
        definition_ref_hash: definition_ref_hash(uid, version),
        inputs,
        outputs,
        interface_root: merkle_root(&leaves),
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
                "version": self.version,
                "definitionRefHash": word_hex(&self.definition_ref_hash),
            },
            "inputs": self.inputs.iter().map(|port| json!({
                "port": port.port,
                "kind": port.kind,
                "stageIdentifier": port.stage_identifier,
                "hookName": port.hook_name,
                "hookId": port.hook_id,
                "canonicalInputSignal": port.canonical_input_signal,
                "canonicalInputSignalHash": word_hex(&port.canonical_input_signal_hash),
                "source": port.source,
                "sourceId": word_hex(&port.source_id),
                "signalId": word_hex(&port.signal_id),
                "accessPolicy": port.access_policy,
                "leafHash": word_hex(&port.leaf_hash),
            })).collect::<Vec<_>>(),
            "outputs": self.outputs.iter().map(|port| json!({
                "port": port.port,
                "canonicalOutputSignal": port.canonical_output_signal,
                "canonicalOutputSignalHash": word_hex(&port.canonical_output_signal_hash),
                "source": port.source,
                "sourceId": word_hex(&port.source_id),
                "signalId": word_hex(&port.signal_id),
                "terminal": port.terminal,
                "leafHash": word_hex(&port.leaf_hash),
            })).collect::<Vec<_>>(),
            "interfaceRoot": word_hex(&self.interface_root),
        })
    }

    /// entrance 端口引用的 hook 集合（`<task>.<stage>#<hook>`），供
    /// orderTriggerKind=dock 标记使用。
    pub fn entrance_hook_ids(&self) -> BTreeSet<String> {
        self.inputs
            .iter()
            .filter(|port| port.kind == "entrance")
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
    pub version: String,
    pub definition_ref_hash: Word,
    pub artifact_hash: Word,
    pub published: bool,
    pub interface: DockInterfaceArtifact,
    pub cloud_artifact_id: Option<String>,
    pub evm_plan_id: Option<Word>,
    pub dock_edges: Vec<(String, String)>,
}

#[derive(Debug, Clone)]
pub struct ResolutionManifest {
    pub targets: Vec<ResolutionTarget>,
}

/// 解析 resolution manifest（Store/发布系统或离线 lock 文件提供）。
/// manifest 只做形状解析；完整性校验（root 重算等）在 link 时执行。
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
        let version = entry
            .get("version")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string();
        if zhixu.is_empty() || version.is_empty() {
            issues.push(DockIssue::new(
                "D008",
                &path,
                "zhixu and version are required",
            ));
            continue;
        }
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
        let Some(interface_value) = entry.get("interface") else {
            issues.push(DockIssue::new(
                "D008",
                format!("{path}.interface"),
                "target DockInterfaceArtifact is required",
            ));
            continue;
        };
        let interface = match parse_interface_artifact(interface_value) {
            Ok(interface) => interface,
            Err(mut interface_issues) => {
                for issue in &mut interface_issues {
                    issue.path = format!("{path}.interface.{}", issue.path);
                }
                issues.append(&mut interface_issues);
                continue;
            }
        };
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
            let target_version = edge
                .get("version")
                .and_then(Value::as_str)
                .unwrap_or_default();
            if !target_zhixu.is_empty() && !target_version.is_empty() {
                dock_edges.push((target_zhixu.to_string(), target_version.to_string()));
            }
        }
        targets.push(ResolutionTarget {
            zhixu,
            version,
            definition_ref_hash: definition_ref,
            artifact_hash,
            published,
            interface,
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

fn parse_interface_artifact(value: &Value) -> DockResult<DockInterfaceArtifact> {
    let mut issues = Vec::new();
    let schema = value
        .get("schemaVersion")
        .and_then(Value::as_str)
        .unwrap_or_default();
    if schema != DOCK_INTERFACE_ARTIFACT_SCHEMA_VERSION {
        issues.push(DockIssue::new(
            "D008",
            "schemaVersion",
            format!("must be \"{DOCK_INTERFACE_ARTIFACT_SCHEMA_VERSION}\""),
        ));
    }
    let definition = value.get("definition").cloned().unwrap_or(Value::Null);
    let uid = definition
        .get("uid")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string();
    let version = definition
        .get("version")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string();
    let Some(definition_ref) = definition
        .get("definitionRefHash")
        .and_then(Value::as_str)
        .and_then(word_from_hex)
    else {
        issues.push(DockIssue::new(
            "D008",
            "definition.definitionRefHash",
            "must be 0x-prefixed bytes32",
        ));
        return Err(issues);
    };
    let mut inputs = Vec::new();
    let mut outputs = Vec::new();
    let mut leaves = Vec::new();
    for port in value
        .get("inputs")
        .and_then(Value::as_array)
        .unwrap_or(&Vec::new())
    {
        let leaf = port
            .get("leafHash")
            .and_then(Value::as_str)
            .and_then(word_from_hex);
        let (Some(leaf_hash), Some(canonical_hash)) = (
            leaf,
            port.get("canonicalInputSignalHash")
                .and_then(Value::as_str)
                .and_then(word_from_hex),
        ) else {
            issues.push(DockIssue::new(
                "D008",
                format!(
                    "inputs.{}",
                    port.get("port").and_then(Value::as_str).unwrap_or("?")
                ),
                "leafHash/canonicalInputSignalHash must be bytes32",
            ));
            continue;
        };
        leaves.push(leaf_hash);
        inputs.push(DockInterfaceArtifactPortInput {
            port: port
                .get("port")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string(),
            kind: port
                .get("kind")
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
            canonical_input_signal_hash: canonical_hash,
            source: port
                .get("source")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string(),
            source_id: port
                .get("sourceId")
                .and_then(Value::as_str)
                .and_then(word_from_hex)
                .ok_or_else(|| {
                    issues.push(DockIssue::new(
                        "D008",
                        format!(
                            "inputs.{}",
                            port.get("port").and_then(Value::as_str).unwrap_or("?")
                        ),
                        "sourceId must be a 0x-prefixed bytes32 word",
                    ));
                    [0u8; 32]
                })
                .unwrap_or_default(),
            signal_id: port
                .get("signalId")
                .and_then(Value::as_str)
                .and_then(word_from_hex)
                .ok_or_else(|| {
                    issues.push(DockIssue::new(
                        "D008",
                        format!(
                            "inputs.{}",
                            port.get("port").and_then(Value::as_str).unwrap_or("?")
                        ),
                        "signalId must be a 0x-prefixed bytes32 word",
                    ));
                    [0u8; 32]
                })
                .unwrap_or_default(),
            access_policy: port
                .get("accessPolicy")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string(),
            leaf_hash,
        });
    }
    for port in value
        .get("outputs")
        .and_then(Value::as_array)
        .unwrap_or(&Vec::new())
    {
        let (Some(leaf_hash), Some(canonical_hash)) = (
            port.get("leafHash")
                .and_then(Value::as_str)
                .and_then(word_from_hex),
            port.get("canonicalOutputSignalHash")
                .and_then(Value::as_str)
                .and_then(word_from_hex),
        ) else {
            issues.push(DockIssue::new(
                "D008",
                format!(
                    "outputs.{}",
                    port.get("port").and_then(Value::as_str).unwrap_or("?")
                ),
                "leafHash/canonicalOutputSignalHash must be bytes32",
            ));
            continue;
        };
        leaves.push(leaf_hash);
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
            canonical_output_signal_hash: canonical_hash,
            source: port
                .get("source")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string(),
            source_id: port
                .get("sourceId")
                .and_then(Value::as_str)
                .and_then(word_from_hex)
                .unwrap_or([0u8; 32]),
            signal_id: port
                .get("signalId")
                .and_then(Value::as_str)
                .and_then(word_from_hex)
                .unwrap_or([0u8; 32]),
            terminal: port
                .get("terminal")
                .and_then(Value::as_str)
                .unwrap_or("none")
                .to_string(),
            leaf_hash,
        });
    }
    let Some(interface_root) = value
        .get("interfaceRoot")
        .and_then(Value::as_str)
        .and_then(word_from_hex)
    else {
        issues.push(DockIssue::new(
            "D008",
            "interfaceRoot",
            "must be 0x-prefixed bytes32",
        ));
        return Err(issues);
    };
    if !issues.is_empty() {
        return Err(issues);
    }
    Ok(DockInterfaceArtifact {
        uid,
        version,
        definition_ref_hash: definition_ref,
        inputs,
        outputs,
        interface_root,
    })
}

// ---------------------------------------------------------------------------
// 已解析 DockRouteV1
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
    pub kind: String,
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
    pub terminal: String,
    pub binding_hash: Word,
}

#[derive(Debug, Clone)]
pub struct DockRoute {
    pub route_id: Word,
    /// Cloud runtime 可读名称（不参与哈希）。
    pub entrance_target_stage_identifier: String,
    pub entrance_target_signal_name: String,
    pub local_definition_ref_hash: Word,
    pub stage_identifier: String,
    pub stage_key: Word,
    pub target_definition_ref_hash: Word,
    pub target_zhixu_uid: String,
    pub target_version: String,
    pub target_artifact_hash: Word,
    pub target_cloud_artifact_id: Option<String>,
    pub target_evm_plan_id: Option<Word>,
    pub target_interface_root: Word,
    pub source_seam: String,
    pub entrance_local_hook_name: String,
    pub entrance_target_port: String,
    pub entrance_target_stage_key: Word,
    pub entrance_target_hook_key: Word,
    pub entrance_target_input_signal_hash: Word,
    pub entrance_access_policy: String,
    pub entrance_binding_hash: Word,
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
                "version": self.target_version,
                "artifactHash": word_hex(&self.target_artifact_hash),
                "cloudArtifactId": self.target_cloud_artifact_id.clone(),
                "evmPlanId": self.target_evm_plan_id.map(|word| word_hex(&word)),
                "interfaceRoot": word_hex(&self.target_interface_root),
            },
            "orderIdPolicy": "derived-v1",
            "sourceSeam": self.source_seam,
            "entrance": {
                "localHookName": self.entrance_local_hook_name,
                "targetPort": self.entrance_target_port,
                "targetStageIdentifier": self.entrance_target_stage_identifier,
                "targetSignalName": self.entrance_target_signal_name,
                "targetStageKey": word_hex(&self.entrance_target_stage_key),
                "targetHookKey": word_hex(&self.entrance_target_hook_key),
                "targetInputSignalHash": word_hex(&self.entrance_target_input_signal_hash),
                "accessPolicy": self.entrance_access_policy,
            },
            "inputs": self.inputs.iter().map(|input| json!({
                "localHookName": input.local_hook_name,
                "targetPort": input.target_port,
                "targetInputSignalHash": word_hex(&input.target_input_signal_hash),
                "targetSourceId": word_hex(&input.target_source_id),
                "targetSignalId": word_hex(&input.target_signal_id),
                "targetStageIdentifier": input.target_stage_identifier,
                "targetSignalName": input.target_signal_name,
                "kind": input.kind,
                "bindingHash": word_hex(&input.binding_hash),
            })).collect::<Vec<_>>(),
            "outputs": self.outputs.iter().map(|output| json!({
                "localSignalName": output.local_signal_name,
                "localSourceId": word_hex(&output.local_source_id),
                "localSignalId": word_hex(&output.local_signal_id),
                "targetPort": output.target_port,
                "targetOutputSignalHash": word_hex(&output.target_output_signal_hash),
                "targetSourceId": word_hex(&output.target_source_id),
                "targetSignalId": word_hex(&output.target_signal_id),
                "targetSignalName": output.target_signal_name,
                "terminal": output.terminal,
                "bindingHash": word_hex(&output.binding_hash),
            })).collect::<Vec<_>>(),
            "inputsRoot": word_hex(&self.inputs_root),
            "outputsRoot": word_hex(&self.outputs_root),
            "routeHash": word_hex(&self.route_hash),
        })
    }
}

/// 父定义本地身份（link 输入）。
pub struct LocalLinkIdentity {
    pub uid: String,
    pub version: String,
}

/// Link：本地未链接 routes + resolution manifest → 已解析 DockRouteV1
/// 列表（D008-D018）。纯函数，无网络、无 I/O。
pub fn link_dock_routes(
    local: &LocalLinkIdentity,
    stages: &[(String, ZhixuStage)],
    unlinked: &[UnlinkedDockRoute],
    manifest: &ResolutionManifest,
) -> DockResult<Vec<DockRoute>> {
    let mut issues = Vec::new();
    let local_definition_ref = definition_ref_hash(&local.uid, &local.version);
    let stages_by_identifier: BTreeMap<&str, &ZhixuStage> = stages
        .iter()
        .map(|(identifier, stage)| (identifier.as_str(), stage))
        .collect();

    // D008：目标 artifact 存在、已发布且完整。interfaceRoot 必须由逐叶子
    // preimage 重算得出（叶子内容与 leafHash 复绑），入口级 definitionRefHash
    // 必须与接口 definition 块一致——自不一致的 manifest 在 link 即拒绝，
    // 不推迟到链上 open 才失败。
    for target in &manifest.targets {
        if target.definition_ref_hash != target.interface.definition_ref_hash {
            issues.push(DockIssue::new(
                "D008",
                format!("resolutionManifest.definitions[{}].definitionRefHash", target.zhixu),
                "entry-level definitionRefHash does not match interface.definition.definitionRefHash",
            ));
        }
        let mut leaves: Vec<Word> = Vec::new();
        for port in &target.interface.inputs {
            let (Some(kind_word), Some(access_word)) = (
                enum_word(&port.kind, &INPUT_KIND_TABLE),
                enum_word(&port.access_policy, &ACCESS_TABLE),
            ) else {
                issues.push(DockIssue::new(
                    "D008",
                    format!(
                        "resolutionManifest.definitions[{}].interface.inputs.{}",
                        target.zhixu, port.port
                    ),
                    format!(
                        "unknown input port kind {:?} or access policy {:?}",
                        port.kind, port.access_policy
                    ),
                ));
                continue;
            };
            let leaf = keccak_words(
                DOMAIN_INTERFACE_INPUT,
                &[
                    target.interface.definition_ref_hash,
                    port_key(&port.port),
                    kind_word,
                    hook_key(&port.hook_id),
                    port.source_id,
                    port.signal_id,
                    access_word,
                ],
            );
            if leaf != port.leaf_hash {
                issues.push(DockIssue::new(
                    "D008",
                    format!(
                        "resolutionManifest.definitions[{}].interface.inputs.{}",
                        target.zhixu, port.port
                    ),
                    "leafHash does not match the recomputed input-port preimage",
                ));
            }
            leaves.push(port.leaf_hash);
        }
        for port in &target.interface.outputs {
            let Some(terminal_word) = enum_word(&port.terminal, &TERMINAL_TABLE) else {
                issues.push(DockIssue::new(
                    "D008",
                    format!(
                        "resolutionManifest.definitions[{}].interface.outputs.{}",
                        target.zhixu, port.port
                    ),
                    format!("unknown terminal {:?}", port.terminal),
                ));
                continue;
            };
            let leaf = keccak_words(
                DOMAIN_INTERFACE_OUTPUT,
                &[
                    target.interface.definition_ref_hash,
                    port_key(&port.port),
                    port.source_id,
                    port.signal_id,
                    terminal_word,
                ],
            );
            if leaf != port.leaf_hash {
                issues.push(DockIssue::new(
                    "D008",
                    format!(
                        "resolutionManifest.definitions[{}].interface.outputs.{}",
                        target.zhixu, port.port
                    ),
                    "leafHash does not match the recomputed output-port preimage",
                ));
            }
            leaves.push(port.leaf_hash);
        }
        let recomputed = {
            leaves.sort_unstable();
            leaves.dedup();
            merkle_root(&leaves)
        };
        if recomputed != target.interface.interface_root {
            issues.push(DockIssue::new(
                "D008",
                format!(
                    "resolutionManifest.definitions[{}].interface.interfaceRoot",
                    target.zhixu
                ),
                "interfaceRoot does not match recomputed root over port leaves",
            ));
        }
    }

    let find_target = |zhixu: &str, version: &str| -> Option<&ResolutionTarget> {
        manifest
            .targets
            .iter()
            .find(|target| target.zhixu == zhixu && target.version == version)
    };

    let mut routes = Vec::new();
    for route in unlinked {
        let config = &route.config;
        let path = format!("{}.executor.zhixuExecutorConfig", route.stage_identifier);
        let Some(target) = find_target(&config.target_zhixu, &config.target_version) else {
            issues.push(DockIssue::new(
                "D008",
                format!("{path}.target"),
                format!(
                    "resolution manifest has no published artifact for {}@{}",
                    config.target_zhixu, config.target_version
                ),
            ));
            continue;
        };
        if !target.published || target.artifact_hash == [0u8; 32] {
            issues.push(DockIssue::new(
                "D008",
                format!("{path}.target"),
                format!(
                    "target artifact {}@{} is not published/immutable",
                    config.target_zhixu, config.target_version
                ),
            ));
            continue;
        }

        // D009/D010/D011：端口存在、方向正确、恰好一个 entrance。
        let mut resolved_inputs = Vec::new();
        let mut entrances = Vec::new();
        for (local_hook, port_name) in &config.input_map {
            let Some(port) = target
                .interface
                .inputs
                .iter()
                .find(|port| &port.port == port_name)
            else {
                issues.push(DockIssue::new(
                    "D009",
                    format!("{path}.inputMap.{local_hook}"),
                    format!(
                        "target {}@{} has no input port {port_name:?}",
                        config.target_zhixu, config.target_version
                    ),
                ));
                continue;
            };
            let hook_key_word = hook_key(&format!("{}#{local_hook}", route.stage_identifier));
            let binding = keccak_words(
                DOMAIN_INPUT_BINDING,
                &[
                    route_id(&local_definition_ref, &route.stage_key),
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
                kind: port.kind.clone(),
                binding_hash: binding,
            });
            if port.kind == "entrance" {
                entrances.push((local_hook.clone(), port.clone(), binding));
            }
        }
        if entrances.len() != 1 {
            issues.push(DockIssue::new(
                "D010",
                format!("{path}.inputMap"),
                format!(
                    "a route must reference exactly one entrance input port, found {}",
                    entrances.len()
                ),
            ));
            continue;
        }
        for input in &resolved_inputs {
            if input.kind != "entrance" && input.kind != "signal" {
                issues.push(DockIssue::new(
                    "D011",
                    format!("{path}.inputMap.{}", input.local_hook_name),
                    format!("target input port {:?} has unknown kind", input.target_port),
                ));
            }
        }

        // D009（输出方向）。
        let mut resolved_outputs = Vec::new();
        let stage = stages_by_identifier
            .get(route.stage_identifier.as_str())
            .copied()
            .expect("unlinked route stage exists");
        for (local_signal, port_name) in &config.signal_map {
            let Some(port) = target
                .interface
                .outputs
                .iter()
                .find(|port| &port.port == port_name)
            else {
                issues.push(DockIssue::new(
                    "D009",
                    format!("{path}.signalMap.{local_signal}"),
                    format!(
                        "target {}@{} has no output port {port_name:?}",
                        config.target_zhixu, config.target_version
                    ),
                ));
                continue;
            };
            let local_mapped_signal = format!(
                "{}::{}.{}",
                stage.source, route.stage_identifier, local_signal
            );
            let local_source_id = keccak_word(stage.source.as_bytes());
            let local_signal_id =
                keccak_word(format!("{}.{}", route.stage_identifier, local_signal).as_bytes());
            let _ = local_mapped_signal;
            let binding = keccak_words(
                DOMAIN_OUTPUT_BINDING,
                &[
                    route_id(&local_definition_ref, &route.stage_key),
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
                terminal: port.terminal.clone(),
                binding_hash: binding,
            });
        }

        // D012：全部被引用端口同一 source seam。
        let mut seams = BTreeSet::new();
        for input in &resolved_inputs {
            if let Some(port) = target
                .interface
                .inputs
                .iter()
                .find(|port| port.port == input.target_port)
            {
                seams.insert(port.source.clone());
            }
        }
        for output in &resolved_outputs {
            if let Some(port) = target
                .interface
                .outputs
                .iter()
                .find(|port| port.port == output.target_port)
            {
                seams.insert(port.source.clone());
            }
        }
        if seams.len() != 1 {
            issues.push(DockIssue::new(
                "D012",
                &path,
                format!(
                    "all ports referenced by one route must share a single target source seam, found {seams:?}"
                ),
            ));
            continue;
        }
        let source_seam = seams.iter().next().cloned().unwrap_or_default();

        // D016：binding 数量上限。
        if resolved_inputs.len() > MAX_DOCK_INPUTS {
            issues.push(DockIssue::new(
                "D016",
                &path,
                format!(
                    "route references {} input bindings, limit is {MAX_DOCK_INPUTS}",
                    resolved_inputs.len()
                ),
            ));
        }
        if resolved_outputs.len() > MAX_DOCK_OUTPUTS {
            issues.push(DockIssue::new(
                "D016",
                &path,
                format!(
                    "route references {} output bindings, limit is {MAX_DOCK_OUTPUTS}",
                    resolved_outputs.len()
                ),
            ));
        }
        if !issues.is_empty() {
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
        let route_id_word = route_id(&local_definition_ref, &route.stage_key);
        let (entrance_hook, entrance_port, entrance_binding) = entrances[0].clone();
        let access_word =
            enum_word(&entrance_port.access_policy, &ACCESS_TABLE).unwrap_or(u8_word(1));
        // PRD95 §5.2：route leaf 必须提交目标 plan。将 target runtime plan id
        // 纳入 routeHash preimage（云轨缺失时为零 word），使链上 open 的
        // routeHash 重算天然绑定 targetPlanId——keeper 无法换目标 plan。
        let target_plan_word = target.evm_plan_id.unwrap_or([0u8; 32]);
        let route_hash = keccak_words(
            DOMAIN_ROUTE,
            &[
                route_id_word,
                target.definition_ref_hash,
                target.artifact_hash,
                target.interface.interface_root,
                target_plan_word,
                u8_word(0), // idPolicy derived-v1
                keccak_word(source_seam.as_bytes()),
                entrance_binding,
                access_word,
                inputs_root,
                outputs_root,
            ],
        );
        routes.push(DockRoute {
            route_id: route_id_word,
            entrance_target_stage_identifier: entrance_port.stage_identifier.clone(),
            entrance_target_signal_name: entrance_port.canonical_input_signal.clone(),
            local_definition_ref_hash: local_definition_ref,
            stage_identifier: route.stage_identifier.clone(),
            stage_key: route.stage_key,
            target_definition_ref_hash: target.definition_ref_hash,
            target_zhixu_uid: target.zhixu.clone(),
            target_version: target.version.clone(),
            target_artifact_hash: target.artifact_hash,
            target_cloud_artifact_id: target.cloud_artifact_id.clone(),
            target_evm_plan_id: target.evm_plan_id,
            target_interface_root: target.interface.interface_root,
            source_seam,
            entrance_local_hook_name: entrance_hook,
            entrance_target_port: entrance_port.port.clone(),
            entrance_target_stage_key: stage_key(&entrance_port.stage_identifier),
            entrance_target_hook_key: hook_key(&entrance_port.hook_id),
            entrance_target_input_signal_hash: entrance_port.canonical_input_signal_hash,
            entrance_access_policy: entrance_port.access_policy.clone(),
            entrance_binding_hash: entrance_binding,
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

    // D015：route 启动图无环。节点 (zhixu, version)，边为 resolved route
    // 与 manifest 提供的目标自身 dockEdges。
    let mut edges: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
    let local_node = format!("{}@{}", local.uid, local.version);
    for target in &manifest.targets {
        let node = format!("{}@{}", target.zhixu, target.version);
        for (zhixu, version) in &target.dock_edges {
            edges
                .entry(node.clone())
                .or_default()
                .insert(format!("{zhixu}@{version}"));
        }
    }
    for route in &routes {
        edges.entry(local_node.clone()).or_default().insert(format!(
            "{}@{}",
            route.target_zhixu_uid, route.target_version
        ));
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

pub const PERMIT_TYPEHASH_SUFFIX: &str = "UVPDockEntrancePermitV1(bytes32 targetPlanId,bytes32 targetEntrancePortId,bytes32 localPlanId,bytes32 routeHash,bytes32 dockInstanceId,bytes32 linkedOrderId,address creator,uint256 feeLimit,uint256 nonce,uint256 deadline)";

pub fn eip712_permit_domain_separator(chain_id: u64, verifying_contract: &str) -> Option<Word> {
    let address = address_word(verifying_contract)?;
    Some(keccak_words(
        "EIP712Domain(string name,string version,uint256 chainId,address verifyingContract)",
        &[
            keccak_word(b"UVPDockingModule"),
            keccak_word(b"2"),
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
    local_plan_id: &Word,
    route_hash: &Word,
    dock_instance: &Word,
    linked_order: &Word,
    creator: &str,
    nonce: u64,
    deadline: u64,
) -> Option<Word> {
    let creator_word = address_word(creator)?;
    let fee_limit = 0u64; // PRD96 §15.5：无费用机制，feeLimit 固定 0（与合约一致）
    let mut buf = Vec::with_capacity(32 * 11);
    buf.extend_from_slice(&keccak_word(PERMIT_TYPEHASH_SUFFIX.as_bytes()));
    for word in [
        target_plan_id,
        target_entrance_port_id,
        local_plan_id,
        route_hash,
        dock_instance,
        linked_order,
    ] {
        buf.extend_from_slice(word);
    }
    buf.extend_from_slice(&creator_word);
    buf.extend_from_slice(&u64_word(fee_limit));
    buf.extend_from_slice(&u64_word(nonce));
    buf.extend_from_slice(&u64_word(deadline));
    Some(keccak_word(&buf))
}

#[allow(clippy::too_many_arguments)]
pub fn eip712_permit_digest(
    chain_id: u64,
    verifying_contract: &str,
    target_plan_id: &Word,
    target_entrance_port_id: &Word,
    local_plan_id: &Word,
    route_hash: &Word,
    dock_instance: &Word,
    linked_order: &Word,
    creator: &str,
    nonce: u64,
    deadline: u64,
) -> Option<Word> {
    let domain_separator = eip712_permit_domain_separator(chain_id, verifying_contract)?;
    let struct_hash = eip712_permit_struct_hash(
        target_plan_id,
        target_entrance_port_id,
        local_plan_id,
        route_hash,
        dock_instance,
        linked_order,
        creator,
        nonce,
        deadline,
    )?;
    let mut buf = Vec::with_capacity(2 + 64);
    buf.extend_from_slice(b"\x19\x01");
    buf.extend_from_slice(&domain_separator);
    buf.extend_from_slice(&struct_hash);
    Some(keccak_word(&buf))
}

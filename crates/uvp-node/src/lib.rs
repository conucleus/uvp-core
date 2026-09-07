use napi::{Error, Result};
use napi_derive::napi;
use std::panic::{catch_unwind, AssertUnwindSafe};

fn run_json(operation: &str, produce: impl FnOnce() -> String) -> Result<String> {
    catch_unwind(AssertUnwindSafe(produce)).map_err(|payload| {
        let detail = payload
            .downcast_ref::<&str>()
            .map(|message| (*message).to_string())
            .or_else(|| payload.downcast_ref::<String>().cloned())
            .unwrap_or_else(|| "unknown panic".to_string());
        Error::from_reason(format!("{operation} panicked: {detail}"))
    })
}

#[napi]
pub fn compile_json(request_json: String) -> Result<String> {
    run_json("compileJson", || uvp_compiler::compile_json(&request_json))
}

#[napi]
pub fn parse_hook_json(request_json: String) -> Result<String> {
    run_json("parseHookJson", || {
        uvp_hook_dsl::parse_hook_json(&request_json)
    })
}

#[napi]
pub fn eval_compiled_hook_json(request_json: String) -> Result<String> {
    run_json("evalCompiledHookJson", || {
        uvp_hook_dsl::eval_compiled_hook_json(&request_json)
    })
}

#[napi]
pub fn replay_json(request_json: String) -> Result<String> {
    run_json("replayJson", || uvp_replay::replay_json(&request_json))
}

#[napi]
pub fn version() -> String {
    uvp_hook_dsl::CORE_VERSION.to_string()
}

#[napi]
pub fn semantic_version() -> String {
    uvp_hook_dsl::SEMANTIC_VERSION.to_string()
}

#[napi]
/// HookPlan 产物信封版本（uvp.hookPlan.v2）：TS 侧兼容门与 uvp-protocol
/// compiler 的 HOOK_PLAN_SCHEMA_VERSION 逐字比对，防两轨信封版本漂移。
pub fn hook_plan_schema_version() -> String {
    uvp_compiler::HOOK_PLAN_SCHEMA_VERSION.to_string()
}

#[napi]
/// 构建指纹（git-<rev>，build.rs 编译期烧入）：TS 侧据此比对当前 uvp-core
/// 检出 HEAD，识别"版本+语义探针双检都放行但行为已变"的陈旧 dylib。
/// `no-git-` 前缀表示构建时找不到 git 仓库，宿主侧应拒绝静默通过。
pub fn build_fingerprint() -> String {
    env!("UVP_BUILD_FINGERPRINT").to_string()
}

#[cfg(test)]
mod tests {
    #[test]
    fn schema_version_exports_match_the_ts_authority_literals() {
        // TS 兼容门消费 NAPI 导出的逐字面量；Rust 侧钉住两个权威字面量，
        // 防止导出面与编译器产物 schemaVersion 漂移。
        assert_eq!(uvp_hook_dsl::SEMANTIC_VERSION, "uvp.semantic.v1");
        assert_eq!(
            uvp_compiler::HOOK_PLAN_SCHEMA_VERSION,
            "uvp.hookPlan.v2",
            "TS authority literal is uvp.protocol compiler HOOK_PLAN_SCHEMA_VERSION"
        );
        assert_eq!(super::hook_plan_schema_version(), "uvp.hookPlan.v2");
    }
}

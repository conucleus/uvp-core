use napi_derive::napi;

#[napi]
pub fn compile_json(request_json: String) -> String {
    uvp_compiler::compile_json(&request_json)
}

#[napi]
pub fn parse_hook_json(request_json: String) -> String {
    uvp_hook_dsl::parse_hook_json(&request_json)
}

#[napi]
pub fn eval_hook_json(request_json: String) -> String {
    uvp_hook_dsl::eval_hook_json(&request_json)
}

#[napi]
pub fn replay_json(request_json: String) -> String {
    uvp_replay::replay_json(&request_json)
}

#[napi]
pub fn version() -> String {
    uvp_hook_dsl::CORE_VERSION.to_string()
}

use std::ffi::{CStr, CString};
use std::os::raw::c_char;
use std::panic::{catch_unwind, AssertUnwindSafe};

fn to_rust_string(ptr: *const c_char) -> String {
    if ptr.is_null() {
        return String::new();
    }
    unsafe { CStr::from_ptr(ptr) }
        .to_string_lossy()
        .into_owned()
}

fn into_c_string(value: String) -> *mut c_char {
    // 输出理论上是 serde 序列化的 JSON 信封（控制字符已被转义），但毒
    // 输入渗入诊断串时可能携带裸 NUL。此处 panic 会在 guard 之外跨
    // extern "C" 边界 abort 宿主进程——把 NUL 就地转义为 JSON 转义形态，
    // 保持输出仍是合法 JSON 且转换永不失败。
    let mut bytes = value.replace('\0', "\\u0000").into_bytes();
    bytes.push(0);
    // SAFETY: NUL 已全部转义，bytes 仅在末尾携带一个 NUL 终止符。
    unsafe { CString::from_vec_unchecked(bytes) }.into_raw()
}

// panic 跨 extern "C" 边界会直接 abort 宿主进程（statemachine），所以每个
// 导出入口都必须把未预期 panic 拦下来，降级成 ok:false 的错误 envelope。
// panic 信封带独立的 panicked:true 结构化字段：宿主靠它把引擎 panic 与调用
// 方输入拒绝分流，不依赖诊断文本的子串匹配（拒绝文案可能嵌入输入内容，
// 子串匹配会误中）。
fn guard_ffi_panic(operation: &str, action: impl FnOnce() -> String) -> String {
    match catch_unwind(AssertUnwindSafe(action)) {
        Ok(output) => output,
        Err(payload) => {
            let message = if let Some(text) = payload.downcast_ref::<&str>() {
                (*text).to_string()
            } else if let Some(text) = payload.downcast_ref::<String>() {
                text.clone()
            } else {
                "unknown panic payload".to_string()
            };
            serde_json::json!({
                "ok": false,
                "panicked": true,
                "diagnostics": [
                    { "message": format!("{operation} panicked: {message}") }
                ]
            })
            .to_string()
        }
    }
}

#[no_mangle]
pub extern "C" fn uvp_compile_json(request_json: *const c_char) -> *mut c_char {
    into_c_string(guard_ffi_panic("uvp_compile_json", || {
        uvp_compiler::compile_json(&to_rust_string(request_json))
    }))
}

#[no_mangle]
pub extern "C" fn uvp_parse_hook_json(request_json: *const c_char) -> *mut c_char {
    into_c_string(guard_ffi_panic("uvp_parse_hook_json", || {
        uvp_hook_dsl::parse_hook_json(&to_rust_string(request_json))
    }))
}

#[no_mangle]
pub extern "C" fn uvp_eval_compiled_hook_json(request_json: *const c_char) -> *mut c_char {
    into_c_string(guard_ffi_panic("uvp_eval_compiled_hook_json", || {
        uvp_hook_dsl::eval_compiled_hook_json(&to_rust_string(request_json))
    }))
}

#[no_mangle]
pub extern "C" fn uvp_replay_json(request_json: *const c_char) -> *mut c_char {
    into_c_string(guard_ffi_panic("uvp_replay_json", || {
        uvp_replay::replay_json(&to_rust_string(request_json))
    }))
}

#[no_mangle]
pub extern "C" fn uvp_lint_hook_json(request_json: *const c_char) -> *mut c_char {
    into_c_string(guard_ffi_panic("uvp_lint_hook_json", || {
        uvp_hook_dsl::lint_hook_json(&to_rust_string(request_json))
    }))
}

#[no_mangle]
pub extern "C" fn uvp_lint_zhixu_json(request_json: *const c_char) -> *mut c_char {
    into_c_string(guard_ffi_panic("uvp_lint_zhixu_json", || {
        uvp_compiler::lint::lint_zhixu_json(&to_rust_string(request_json))
    }))
}

#[no_mangle]
/// # Safety
///
/// `ptr` must be a non-null pointer returned by one of this library's JSON
/// functions, and it must not have been freed before. Passing any other pointer
/// is undefined behavior.
pub unsafe extern "C" fn uvp_free(ptr: *mut c_char) {
    if ptr.is_null() {
        return;
    }
    let _ = CString::from_raw(ptr);
}

#[no_mangle]
pub extern "C" fn uvp_core_version() -> *const c_char {
    static VERSION: &str = concat!(env!("CARGO_PKG_VERSION"), "\0");
    VERSION.as_ptr() as *const c_char
}

#[no_mangle]
/// 语义版本（uvp.semantic.v1 线）：宿主侧版本协商应直接读取本导出，而
/// 不是在二进制版本不匹配时打印占位符（如 "<unknown>"）——语义版本是
/// 编译产物口径的权威标识。concat! 不接受 const，用 OnceLock 钉一份
/// 带 NUL 的静态缓冲。
pub extern "C" fn uvp_core_semantic_version() -> *const c_char {
    static BUFFER: std::sync::OnceLock<Box<[u8]>> = std::sync::OnceLock::new();
    let buffer = BUFFER.get_or_init(|| {
        let mut bytes = uvp_hook_dsl::SEMANTIC_VERSION.as_bytes().to_vec();
        bytes.push(0);
        bytes.into_boxed_slice()
    });
    buffer.as_ptr() as *const c_char
}

#[no_mangle]
/// 构建指纹（由 build.rs 烧入，形如 `git-<rev>`）：宿主语言据此识别陈旧
/// FFI 产物——语义版本不变而行为已变的旧构建无法被版本+语义探针拦住，
/// 指纹比对是最终防线。`no-git-` 前缀表示构建时找不到 git 仓库，宿主侧
/// 应拒绝静默通过。
pub extern "C" fn uvp_core_build_fingerprint() -> *const c_char {
    static FINGERPRINT: &str = concat!(env!("UVP_BUILD_FINGERPRINT"), "\0");
    FINGERPRINT.as_ptr() as *const c_char
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn nul_in_output_is_escaped_not_panicked() {
        // 毒诊断串携带裸 NUL：转换必须就地把 NUL 转成 JSON 转义形态，
        // 而不是在 panic guard 之外 panic 跨 extern "C" 边界 abort 宿主。
        let ptr = into_c_string("{\"message\":\"a\0b\"}".to_string());
        let recovered = unsafe { CString::from_raw(ptr) };
        assert_eq!(recovered.to_str().unwrap(), "{\"message\":\"a\\u0000b\"}");
    }

    #[test]
    fn panic_envelope_carries_structured_panicked_field() {
        // 宿主（Go callJSON）靠 panicked:true 结构化字段分流引擎 panic 与
        // 调用方输入拒绝：诊断文本里的 "panicked:" 子串可被输入内容伪造，
        // 不能作为判据。
        let raw = guard_ffi_panic("uvp_compile_json", || panic!("boom"));
        let value: serde_json::Value = serde_json::from_str(&raw).unwrap();
        assert_eq!(value["ok"], serde_json::json!(false));
        assert_eq!(value["panicked"], serde_json::json!(true));
        assert!(value["diagnostics"][0]["message"]
            .as_str()
            .unwrap()
            .starts_with("uvp_compile_json panicked:"));
    }
}

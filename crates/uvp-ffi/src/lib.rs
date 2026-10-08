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
    let mut bytes = value.replace('\0', "\\u0000").into_bytes();
    bytes.push(0);
    unsafe { CString::from_vec_unchecked(bytes) }.into_raw()
}

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
pub extern "C" fn uvp_replay_compiled_hook_json(request_json: *const c_char) -> *mut c_char {
    into_c_string(guard_ffi_panic("uvp_replay_compiled_hook_json", || {
        uvp_replay::replay_compiled_hook_json(&to_rust_string(request_json))
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
pub extern "C" fn uvp_derive_definition_uid_json(definition_json: *const c_char) -> *mut c_char {
    into_c_string(guard_ffi_panic(
        "uvp_derive_definition_uid_json",
        || match serde_json::from_str::<serde_json::Value>(&to_rust_string(definition_json)) {
            Ok(definition) => match uvp_ir::derive_definition_uid(&definition) {
                Ok(uid) => serde_json::json!({ "ok": true, "value": uid }).to_string(),
                Err(err) => serde_json::json!({
                    "ok": false,
                    "diagnostics": [{ "message": err.to_string() }]
                })
                .to_string(),
            },
            Err(err) => serde_json::json!({
                "ok": false,
                "diagnostics": [{ "message": format!("definition is not valid JSON: {err}") }]
            })
            .to_string(),
        },
    ))
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
pub extern "C" fn uvp_core_build_fingerprint() -> *const c_char {
    static FINGERPRINT: &str = concat!(env!("UVP_BUILD_FINGERPRINT"), "\0");
    FINGERPRINT.as_ptr() as *const c_char
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn nul_in_output_is_escaped_not_panicked() {
        let ptr = into_c_string("{\"message\":\"a\0b\"}".to_string());
        let recovered = unsafe { CString::from_raw(ptr) };
        assert_eq!(recovered.to_str().unwrap(), "{\"message\":\"a\\u0000b\"}");
    }

    #[test]
    fn panic_envelope_carries_structured_panicked_field() {
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

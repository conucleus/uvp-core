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
    CString::new(value)
        .expect("JSON output should not contain NUL bytes")
        .into_raw()
}

// panic 跨 extern "C" 边界会直接 abort 宿主进程（statemachine），所以每个
// 导出入口都必须把未预期 panic 拦下来，降级成 ok:false 的错误 envelope。
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

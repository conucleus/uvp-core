use std::ffi::{CStr, CString};
use std::os::raw::c_char;

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

#[no_mangle]
pub extern "C" fn uvp_compile_json(request_json: *const c_char) -> *mut c_char {
    into_c_string(uvp_compiler::compile_json(&to_rust_string(request_json)))
}

#[no_mangle]
pub extern "C" fn uvp_parse_hook_json(request_json: *const c_char) -> *mut c_char {
    into_c_string(uvp_hook_dsl::parse_hook_json(&to_rust_string(request_json)))
}

#[no_mangle]
pub extern "C" fn uvp_eval_hook_json(request_json: *const c_char) -> *mut c_char {
    into_c_string(uvp_hook_dsl::eval_hook_json(&to_rust_string(request_json)))
}

#[no_mangle]
pub extern "C" fn uvp_replay_json(request_json: *const c_char) -> *mut c_char {
    into_c_string(uvp_replay::replay_json(&to_rust_string(request_json)))
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

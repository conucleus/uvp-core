//! macOS 动态查找符号 + 把 uvp-core 的 git rev 在编译期烧进 NAPI 产物
//! （构建指纹，与 uvp-ffi/build.rs 同一套取值规则）。
//!
//! TS 宿主据此识别陈旧 dylib：语义版本不变而行为已变的旧构建无法被
//! 版本+语义探针双检拦住，指纹比对是最终防线。
//!
//! 取值优先级：
//! 1. 环境变量 `UVP_FFI_GIT_REV`（hermetic 构建显式指定，与 uvp-ffi 共用
//!    同一变量名——两份产物钉同一个 rev）；
//! 2. 运行 `git rev-parse HEAD` 读取 workspace 根的提交；
//! 3. 都不可用则退化为 `no-git-<CARGO_PKG_VERSION>`，宿主侧据此拒绝静默通过。

use std::path::{Path, PathBuf};

fn main() {
    #[cfg(target_os = "macos")]
    {
        println!("cargo:rustc-link-arg=-undefined");
        println!("cargo:rustc-link-arg=dynamic_lookup");
    }

    let manifest_dir =
        PathBuf::from(std::env::var("CARGO_MANIFEST_DIR").expect("cargo sets CARGO_MANIFEST_DIR"));
    let workspace_root = manifest_dir
        .parent()
        .and_then(Path::parent)
        .map(Path::to_path_buf)
        .expect("uvp-node crate lives two levels below the workspace root");

    emit_git_rerun_triggers(&workspace_root);

    let fingerprint = match std::env::var("UVP_FFI_GIT_REV") {
        Ok(rev) if !rev.trim().is_empty() => format!("git-{}", rev.trim()),
        _ => match git_head_rev(&workspace_root) {
            Some(rev) => format!("git-{rev}"),
            None => format!(
                "no-git-{}",
                std::env::var("CARGO_PKG_VERSION").expect("cargo sets CARGO_PKG_VERSION")
            ),
        },
    };
    println!("cargo:rustc-env=UVP_BUILD_FINGERPRINT={fingerprint}");
}

// git ref 变化（新提交、切分支）必须触发 build script 重跑，否则指纹会停在
// 上次编译时的 rev，TS 侧只能看到过期的"当前"HEAD。
fn emit_git_rerun_triggers(workspace_root: &Path) {
    let Some(git_dir) = resolve_git_dir(workspace_root) else {
        return;
    };
    for trigger in ["HEAD", "refs", "packed-refs"] {
        let path = git_dir.join(trigger);
        if path.exists() {
            println!("cargo:rerun-if-changed={}", path.display());
        }
    }
}

// `.git` 可能是目录（普通克隆）也可能是文件（worktree/submodule 的 gitdir 指针）。
fn resolve_git_dir(workspace_root: &Path) -> Option<PathBuf> {
    let dot_git = workspace_root.join(".git");
    if dot_git.is_dir() {
        return Some(dot_git);
    }
    if dot_git.is_file() {
        let content = std::fs::read_to_string(&dot_git).ok()?;
        let gitdir = content.trim().strip_prefix("gitdir:")?.trim();
        let path = PathBuf::from(gitdir);
        return Some(if path.is_absolute() {
            path
        } else {
            workspace_root.join(path)
        });
    }
    None
}

fn git_head_rev(workspace_root: &Path) -> Option<String> {
    let output = std::process::Command::new("git")
        .arg("-C")
        .arg(workspace_root)
        .args(["rev-parse", "HEAD"])
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let rev = String::from_utf8(output.stdout).ok()?.trim().to_string();
    if rev.is_empty() {
        None
    } else {
        Some(rev)
    }
}

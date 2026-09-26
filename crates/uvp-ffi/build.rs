//! 把 uvp-core 的内容树哈希在编译期烧进 uvp-ffi 二进制（构建指纹）。
//!
//! 构建指纹是产物身份的最终依据：语义版本常量不变而行为已变的陈旧
//! 构建，能同时骗过宿主侧的"二进制版本 + 语义探针"双检；宿主侧必须
//! 比对指纹与当前 uvp-core 检出的内容树，不一致即判定陈旧产物并响亮
//! 报错。指纹取内容树（`HEAD^{tree}`）而非提交 SHA：本仓 dev→main
//! 惯例是 squash 收敛，会以内容完全相同的新提交改写全部 SHA，提交
//! SHA 指纹会把这类检出误判为陈旧并强迫重编；树哈希下内容变则指纹
//! 变，squash 只改提交图不误伤。
//!
//! 取值优先级：
//! 1. 环境变量 `UVP_FFI_GIT_REV`（hermetic 构建显式指定）；
//! 2. 运行 `git rev-parse HEAD^{tree}` 读取 workspace 根提交的内容树；
//! 3. 都不可用则退化为 `no-git-<CARGO_PKG_VERSION>`，宿主侧据此拒绝静默通过。

use std::path::{Path, PathBuf};
use std::process::Command;

fn main() {
    println!("cargo:rerun-if-env-changed=UVP_FFI_GIT_REV");
    let manifest_dir =
        PathBuf::from(std::env::var("CARGO_MANIFEST_DIR").expect("cargo sets CARGO_MANIFEST_DIR"));
    let workspace_root = manifest_dir
        .parent()
        .and_then(Path::parent)
        .map(Path::to_path_buf)
        .expect("uvp-ffi crate lives two levels below the workspace root");

    emit_git_rerun_triggers(&workspace_root);

    let fingerprint = match std::env::var("UVP_FFI_GIT_REV") {
        Ok(rev) if !rev.trim().is_empty() => format!("git-{}", rev.trim()),
        _ => match git_head_content_tree(&workspace_root) {
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
// 上次编译时的 rev，钉测试只能看到过期的"当前"内容树。
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

fn git_head_content_tree(workspace_root: &Path) -> Option<String> {
    let output = Command::new("git")
        .arg("-C")
        .arg(workspace_root)
        .args(["rev-parse", "HEAD^{tree}"])
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

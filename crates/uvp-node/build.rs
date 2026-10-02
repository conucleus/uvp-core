//! macOS 动态查找符号 + 把 uvp-core 的内容树哈希在编译期烧进 NAPI 产物
//! （构建指纹，与 uvp-ffi/build.rs 同一套取值规则）。

use std::path::{Path, PathBuf};

fn main() {
    #[cfg(target_os = "macos")]
    {
        println!("cargo:rustc-link-arg=-undefined");
        println!("cargo:rustc-link-arg=dynamic_lookup");
    }
    println!("cargo:rerun-if-env-changed=UVP_FFI_GIT_REV");

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
    let output = std::process::Command::new("git")
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

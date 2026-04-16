use std::path::Path;
use std::path::PathBuf;
use std::process::Command;

fn main() {
    let Some(manifest_dir) = std::env::var_os("CARGO_MANIFEST_DIR").map(PathBuf::from) else {
        return;
    };
    let Some(repo_root) = manifest_dir.parent().and_then(Path::parent) else {
        return;
    };

    if let Some(version) = resolve_codex_version(repo_root) {
        println!("cargo:rustc-env=CODEX_RS_RESOLVED_VERSION={version}");
    }
}

fn resolve_codex_version(repo_root: &Path) -> Option<String> {
    version_from_ref(repo_root, "refs/remotes/upstream/latest-alpha-cli")
        .or_else(|| latest_rust_tag(repo_root))
}

fn version_from_ref(repo_root: &Path, git_ref: &str) -> Option<String> {
    let resolved = git_output(
        repo_root,
        [
            "rev-parse",
            "--verify",
            "--quiet",
            &format!("{git_ref}^{{commit}}"),
        ],
    )?;
    if resolved.trim().is_empty() {
        return None;
    }

    let tag = git_output(
        repo_root,
        [
            "describe",
            "--tags",
            "--abbrev=0",
            "--match",
            "rust-v*",
            git_ref,
        ],
    )?;
    normalize_rust_release_tag(tag.trim())
}

fn latest_rust_tag(repo_root: &Path) -> Option<String> {
    let tag = git_output(
        repo_root,
        ["tag", "--sort=-creatordate", "--list", "rust-v*"],
    )?;
    let latest = tag.lines().find(|line| !line.trim().is_empty())?;
    normalize_rust_release_tag(latest.trim())
}

fn normalize_rust_release_tag(tag: &str) -> Option<String> {
    let trimmed = tag
        .strip_prefix("rust-v")
        .unwrap_or(tag)
        .trim_start_matches('.');
    (!trimmed.is_empty()).then(|| trimmed.to_string())
}

fn git_output<const N: usize>(repo_root: &Path, args: [&str; N]) -> Option<String> {
    let output = Command::new("git")
        .arg("-C")
        .arg(repo_root)
        .args(args)
        .output()
        .ok()?;
    output
        .status
        .success()
        .then(|| String::from_utf8_lossy(&output.stdout).to_string())
}

use codex_protocol::user_input::UserInput;
use globset::GlobBuilder;
use std::collections::HashSet;
use std::ffi::OsStr;
use std::path::Component;
use std::path::Path;
use std::path::PathBuf;
use tokio::fs;
use walkdir::WalkDir;

const MAX_GLOB_MATCHES: usize = 64;

#[derive(Debug)]
pub(crate) struct OracleAttachmentResolution {
    pub(crate) prompt: String,
    pub(crate) files: Vec<String>,
}

pub(crate) async fn resolve_oracle_attachments(
    prompt_text: &str,
    items: &[UserInput],
    workspace_cwd: &Path,
) -> Result<OracleAttachmentResolution, String> {
    let workspace_root = canonical_workspace_root(workspace_cwd).await?;
    let mut files = Vec::new();
    let mut seen = HashSet::new();
    for item in items {
        if let UserInput::LocalImage { path } = item {
            push_unique(
                &mut files,
                &mut seen,
                resolve_local_image_file(&workspace_root, path.as_path()).await?,
            );
        }
    }
    let mut prompt_lines = Vec::new();
    let mut in_code_fence = false;
    for line in prompt_text.lines() {
        if !in_code_fence && (line.starts_with("    ") || line.starts_with('\t')) {
            prompt_lines.push(line.to_string());
            continue;
        }
        let trimmed = line.trim_start();
        if trimmed.starts_with("```") {
            in_code_fence = !in_code_fence;
            prompt_lines.push(line.to_string());
            continue;
        }
        if !in_code_fence {
            if let Some(spec) = directive_value(trimmed, "file:", "file")? {
                push_unique(
                    &mut files,
                    &mut seen,
                    resolve_workspace_file(&workspace_root, &spec).await?,
                );
                prompt_lines.push(format!("[local_file: {spec}]"));
                continue;
            }
            if let Some(pattern) = directive_value(trimmed, "glob:", "glob")? {
                for matched in expand_workspace_glob(&workspace_root, &pattern).await? {
                    push_unique(&mut files, &mut seen, matched);
                }
                prompt_lines.push(format!("[local_glob: {pattern}]"));
                continue;
            }
        }
        prompt_lines.push(line.to_string());
    }
    Ok(OracleAttachmentResolution {
        prompt: prompt_lines.join("\n"),
        files,
    })
}

fn directive_value(line: &str, prefix: &str, label: &str) -> Result<Option<String>, String> {
    let Some(raw_value) = line.strip_prefix(prefix) else {
        return Ok(None);
    };
    let value = raw_value.trim();
    if value.is_empty() {
        return Err(format!("{label}: path was empty."));
    }
    if let Some(quoted) = strip_quoted(value) {
        return Ok(Some(quoted.to_string()));
    }
    if value.starts_with('"') || value.starts_with('\'') {
        return Err(format!("{label}: quoted paths must end with the same quote."));
    }
    if value.chars().any(char::is_whitespace) {
        return if looks_like_spaced_attachment_spec(value) {
            Err(format!(
                "{label}: paths with spaces must be quoted to avoid accidental attachment parsing."
            ))
        } else {
            Ok(None)
        };
    }
    if label != "file" && !looks_like_attachment_spec(value) {
        return Ok(None);
    }
    Ok(Some(value.to_string()))
}

fn workspace_spec(spec: &str, label: &str) -> Result<String, String> {
    let trimmed = spec.trim();
    if trimmed.is_empty() {
        return Err(format!("{label}: path was empty."));
    }
    let path = Path::new(trimmed);
    if path.is_absolute() {
        return Err(format!(
            "{label}:{trimmed} was rejected because absolute paths are not allowed."
        ));
    }
    if path.components().any(|component| {
        matches!(
            component,
            Component::ParentDir | Component::RootDir | Component::Prefix(_)
        )
    }) {
        return Err(format!(
            "{label}:{trimmed} was rejected because it escapes the workspace."
        ));
    }
    Ok(trimmed.to_string())
}

async fn canonical_workspace_root(workspace_cwd: &Path) -> Result<PathBuf, String> {
    let workspace_root = fs::canonicalize(workspace_cwd).await.map_err(|error| {
        format!(
            "workspace {} could not be resolved: {error}",
            workspace_cwd.display()
        )
    })?;
    let metadata = fs::metadata(&workspace_root).await.map_err(|error| {
        format!("workspace {} could not be read: {error}", workspace_root.display())
    })?;
    if !metadata.is_dir() {
        return Err(format!(
            "workspace {} was rejected because it is not a directory.",
            workspace_root.display()
        ));
    }
    Ok(workspace_root)
}

async fn resolve_workspace_file(workspace_root: &Path, spec: &str) -> Result<String, String> {
    let spec = workspace_spec(spec, "file")?;
    let candidate = workspace_root.join(&spec);
    let real_path = canonical_file_path(&candidate, "file", &spec).await?;
    if !real_path.starts_with(workspace_root) {
        return Err(format!(
            "file:{spec} was rejected because it resolves outside the workspace."
        ));
    }
    Ok(real_path.display().to_string())
}

async fn expand_workspace_glob(workspace_root: &Path, pattern: &str) -> Result<Vec<String>, String> {
    let pattern = workspace_spec(pattern, "glob")?;
    let matcher = GlobBuilder::new(&pattern)
        .literal_separator(true)
        .build()
        .map_err(|error| format!("glob:{pattern} is invalid: {error}"))?
        .compile_matcher();
    let walk_root = workspace_glob_root(workspace_root, &pattern);
    if !walk_root.exists() {
        return Err(format!("glob:{pattern} matched no files."));
    }
    let mut matched = Vec::new();
    for entry in WalkDir::new(&walk_root)
        .follow_links(false)
        .sort_by_file_name()
        .into_iter()
        .filter_entry(|entry| entry.file_name() != OsStr::new(".git"))
    {
        let entry = entry.map_err(|error| format!("glob:{pattern} failed: {error}"))?;
        if !entry.file_type().is_file() {
            continue;
        }
        let relative = entry
            .path()
            .strip_prefix(workspace_root)
            .map_err(|error| format!("glob:{pattern} failed: {error}"))?;
        if matcher.is_match(relative) {
            matched.push(relative.to_path_buf());
            if matched.len() > MAX_GLOB_MATCHES {
                return Err(format!(
                    "glob:{pattern} matched more than {MAX_GLOB_MATCHES} files. Narrow the pattern or attach a zip instead."
                ));
            }
        }
    }
    if matched.is_empty() {
        return Err(format!("glob:{pattern} matched no files."));
    }
    let mut resolved = Vec::with_capacity(matched.len());
    for relative in matched {
        resolved.push(resolve_workspace_file(workspace_root, &relative.to_string_lossy()).await?);
    }
    Ok(resolved)
}

fn strip_quoted(value: &str) -> Option<&str> {
    value
        .strip_prefix('"')
        .and_then(|inner| inner.strip_suffix('"'))
        .or_else(|| value.strip_prefix('\'').and_then(|inner| inner.strip_suffix('\'')))
}

fn workspace_glob_root(workspace_root: &Path, pattern: &str) -> PathBuf {
    let relative_root = Path::new(pattern)
        .components()
        .take_while(|component| !has_glob_meta(component.as_os_str()))
        .fold(PathBuf::new(), |mut root, component| {
            root.push(component.as_os_str());
            root
        });
    if relative_root.as_os_str().is_empty() {
        workspace_root.to_path_buf()
    } else {
        workspace_root.join(relative_root)
    }
}

fn has_glob_meta(value: &OsStr) -> bool {
    value
        .to_string_lossy()
        .chars()
        .any(|ch| matches!(ch, '*' | '?' | '[' | '{'))
}

fn looks_like_attachment_spec(value: &str) -> bool {
    value.contains('/')
        || value.contains('\\')
        || value.contains('.')
        || value.chars().any(|ch| matches!(ch, '*' | '?' | '[' | '{'))
}

fn looks_like_spaced_attachment_spec(value: &str) -> bool {
    value.contains('/')
        || value.contains('\\')
        || value.chars().any(|ch| matches!(ch, '*' | '?' | '[' | '{'))
}

async fn resolve_local_image_file(workspace_root: &Path, path: &Path) -> Result<String, String> {
    if !path.is_absolute()
        && path.components().any(|component| {
            matches!(
                component,
                Component::ParentDir | Component::RootDir | Component::Prefix(_)
            )
        })
    {
        return Err(format!(
            "local image {} was rejected because it escapes the workspace.",
            path.display()
        ));
    }
    let is_relative = !path.is_absolute();
    let candidate = if is_relative {
        workspace_root.join(path)
    } else {
        path.to_path_buf()
    };
    let real_path =
        canonical_file_path(&candidate, "local image", &candidate.display().to_string()).await?;
    if is_relative && !real_path.starts_with(workspace_root) {
        return Err(format!(
            "local image {} was rejected because it resolves outside the workspace.",
            path.display()
        ));
    }
    Ok(real_path.display().to_string())
}

async fn canonical_file_path(candidate: &Path, label: &str, spec: &str) -> Result<PathBuf, String> {
    let metadata = fs::metadata(candidate)
        .await
        .map_err(|error| format!("{label} {spec} could not be read: {error}"))?;
    if !metadata.is_file() {
        return Err(format!("{label} {spec} was rejected because it is not a file."));
    }
    fs::canonicalize(candidate)
        .await
        .map_err(|error| format!("{label} {spec} could not be resolved: {error}"))
}

fn push_unique(files: &mut Vec<String>, seen: &mut HashSet<String>, value: String) {
    if seen.insert(value.clone()) {
        files.push(value);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn resolves_workspace_file_and_glob_directives() {
        let temp = tempfile::tempdir().expect("tempdir");
        let src = temp.path().join("src");
        std::fs::create_dir_all(&src).expect("mkdir");
        std::fs::write(src.join("a.rs"), "fn a() {}\n").expect("write");
        std::fs::write(src.join("b.rs"), "fn b() {}\n").expect("write");
        let resolved = resolve_oracle_attachments(
            "Review these\nfile: src/a.rs\nglob: src/**/*.rs",
            &[UserInput::LocalImage {
                path: PathBuf::from("src/a.rs"),
            }],
            temp.path(),
        )
        .await
        .expect("resolved");
        assert!(resolved.prompt.contains("[local_file: src/a.rs]"));
        assert!(resolved.prompt.contains("[local_glob: src/**/*.rs]"));
        assert_eq!(resolved.files.len(), 2);
    }

    #[tokio::test]
    async fn ignores_directives_inside_fenced_blocks() {
        let temp = tempfile::tempdir().expect("tempdir");
        let resolved = resolve_oracle_attachments("```text\nfile: src/a.rs\n```", &[], temp.path())
            .await
            .expect("resolved");
        assert_eq!(resolved.prompt, "```text\nfile: src/a.rs\n```");
        assert!(resolved.files.is_empty());
    }

    #[tokio::test]
    async fn rejects_paths_that_escape_the_workspace() {
        let temp = tempfile::tempdir().expect("tempdir");
        let error = resolve_oracle_attachments("file: ../secret.txt", &[], temp.path())
            .await
            .expect_err("expected workspace rejection");
        assert!(error.contains("escapes the workspace"));
    }

    #[tokio::test]
    async fn canonicalizes_and_deduplicates_local_images() {
        let temp = tempfile::tempdir().expect("tempdir");
        let image = temp.path().join("image.png");
        std::fs::write(&image, b"png").expect("write");
        let canonical = std::fs::canonicalize(&image).expect("canonical");
        let resolved = resolve_oracle_attachments(
            "Review image",
            &[
                UserInput::LocalImage {
                    path: PathBuf::from("image.png"),
                },
                UserInput::LocalImage { path: image.clone() },
            ],
            temp.path(),
        )
        .await
        .expect("resolved");
        assert_eq!(resolved.files, vec![canonical.display().to_string()]);
    }

    #[tokio::test]
    async fn rejects_local_images_that_escape_the_workspace_when_relative() {
        let temp = tempfile::tempdir().expect("tempdir");
        let error = resolve_oracle_attachments(
            "Review image",
            &[UserInput::LocalImage {
                path: PathBuf::from("../image.png"),
            }],
            temp.path(),
        )
        .await
        .expect_err("expected rejection");
        assert!(error.contains("escapes the workspace"));
    }

    #[tokio::test]
    async fn quoted_directives_allow_spaces_and_literal_text_stays_text() {
        let temp = tempfile::tempdir().expect("tempdir");
        let docs = temp.path().join("docs");
        std::fs::create_dir_all(&docs).expect("mkdir");
        std::fs::write(docs.join("My File.txt"), "hi\n").expect("write");
        let resolved = resolve_oracle_attachments("file: \"docs/My File.txt\"", &[], temp.path())
            .await
            .expect("resolved");
        assert!(resolved.prompt.contains("[local_file: docs/My File.txt]"));

        let literal = resolve_oracle_attachments(
            "file: please keep this literal syntax in the docs",
            &[],
            temp.path(),
        )
        .await
        .expect("literal text");
        assert_eq!(
            literal.prompt,
            "file: please keep this literal syntax in the docs"
        );
        assert!(literal.files.is_empty());
    }

    #[tokio::test]
    async fn extensionless_file_directives_still_attach() {
        let temp = tempfile::tempdir().expect("tempdir");
        std::fs::write(temp.path().join("Makefile"), "all:\n\ttrue\n").expect("write");
        let resolved = resolve_oracle_attachments("file: Makefile", &[], temp.path())
            .await
            .expect("resolved");
        assert_eq!(resolved.files.len(), 1);
        assert!(resolved.prompt.contains("[local_file: Makefile]"));
    }

    #[tokio::test]
    async fn path_like_literal_prose_with_spaces_stays_text() {
        let temp = tempfile::tempdir().expect("tempdir");
        std::fs::write(temp.path().join("package.json"), "{ }\n").expect("write");
        let resolved = resolve_oracle_attachments(
            "file: package.json is the example to mention in the docs",
            &[],
            temp.path(),
        )
        .await
        .expect("literal text");
        assert_eq!(
            resolved.prompt,
            "file: package.json is the example to mention in the docs"
        );
        assert!(resolved.files.is_empty());
    }

    #[tokio::test]
    async fn rejects_unquoted_paths_with_spaces() {
        let temp = tempfile::tempdir().expect("tempdir");
        let docs = temp.path().join("docs");
        std::fs::create_dir_all(&docs).expect("mkdir");
        std::fs::write(docs.join("My File.txt"), "hi\n").expect("write");
        let error = resolve_oracle_attachments("file: docs/My File.txt", &[], temp.path())
            .await
            .expect_err("expected rejection");
        assert!(error.contains("must be quoted"));
    }

    #[tokio::test]
    async fn glob_matches_gitignored_zip_files() {
        let temp = tempfile::tempdir().expect("tempdir");
        let tmp = temp.path().join("tmp");
        std::fs::create_dir_all(&tmp).expect("mkdir");
        std::fs::write(temp.path().join(".gitignore"), "tmp/*.zip\n").expect("gitignore");
        std::fs::write(tmp.join("context.zip"), b"zip").expect("write");
        let resolved = resolve_oracle_attachments("glob: tmp/*.zip", &[], temp.path())
            .await
            .expect("resolved");
        assert_eq!(resolved.files.len(), 1);
    }

    #[tokio::test]
    async fn rejects_globs_that_match_too_many_files() {
        let temp = tempfile::tempdir().expect("tempdir");
        let many = temp.path().join("many");
        std::fs::create_dir_all(&many).expect("mkdir");
        for index in 0..=MAX_GLOB_MATCHES {
            std::fs::write(many.join(format!("file-{index}.txt")), "x").expect("write");
        }
        let error = resolve_oracle_attachments("glob: many/*.txt", &[], temp.path())
        .await
        .expect_err("expected limit rejection");
        assert!(error.contains("matched more than"));
    }

    #[tokio::test]
    async fn ignores_indented_markdown_code_blocks() {
        let temp = tempfile::tempdir().expect("tempdir");
        let resolved = resolve_oracle_attachments("    file: src/a.rs", &[], temp.path())
            .await
            .expect("resolved");
        assert_eq!(resolved.prompt, "    file: src/a.rs");
        assert!(resolved.files.is_empty());
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn rejects_relative_local_image_symlink_escape() {
        let temp = tempfile::tempdir().expect("tempdir");
        let outside_dir = tempfile::tempdir().expect("outside");
        let outside = outside_dir.path().join("outside-image.png");
        std::fs::write(&outside, b"png").expect("write");
        std::os::unix::fs::symlink(&outside, temp.path().join("escape.png")).expect("symlink");
        let error = resolve_oracle_attachments(
            "Review image",
            &[UserInput::LocalImage {
                path: PathBuf::from("escape.png"),
            }],
            temp.path(),
        )
        .await
        .expect_err("expected symlink rejection");
        assert!(error.contains("outside the workspace"));
    }
}

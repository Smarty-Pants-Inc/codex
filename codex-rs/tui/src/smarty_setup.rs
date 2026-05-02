use std::path::Path;
use std::path::PathBuf;
use std::process::Command;

pub(crate) struct SmartySetupResult {
    pub(crate) message: String,
    pub(crate) hint: Option<String>,
}

pub(crate) fn run(cwd: &Path) -> SmartySetupResult {
    let mut attempts = Vec::new();
    if let Ok(root) = std::env::var("SMARTY_AGENTS_ROOT") {
        let root_path = PathBuf::from(root);
        if root_path.join("src/smarty_agents/cli.py").is_file() {
            let mut command = python_module_command(cwd);
            command.env("PYTHONPATH", pythonpath_with_root(&root_path));
            if root_path.join("catalog").is_dir() && std::env::var_os("SMARTY_CATALOG").is_none() {
                command.env("SMARTY_CATALOG", root_path.join("catalog"));
            }
            attempts.push(command);
        }
    }
    attempts.push(python_module_command(cwd));
    let mut smarty = Command::new("smarty");
    let cwd_arg = cwd.display().to_string();
    smarty.args(["codex", "setup", "--root", &cwd_arg, "--json"]);
    attempts.push(smarty);

    let mut failures = Vec::new();
    for mut command in attempts {
        match command.output() {
            Ok(output) if output.status.success() => {
                return SmartySetupResult {
                    message: format_setup_output(true, &output.stdout, &output.stderr),
                    hint: Some(
                        "Restart `smarty-codex` from the new Smarty-enabled worktree to enter through Smarty Agents."
                            .to_string(),
                    ),
                };
            }
            Ok(output) => failures.push(format_setup_output(false, &output.stdout, &output.stderr)),
            Err(err) => failures.push(format!("failed to launch setup command: {err}")),
        }
    }
    SmartySetupResult {
        message: format!("Smarty setup failed.\n{}", failures.join("\n")),
        hint: Some(
            "Install smarty-agents or set SMARTY_AGENTS_ROOT, then run /smarty again.".to_string(),
        ),
    }
}

fn python_module_command(cwd: &Path) -> Command {
    let mut command = Command::new("python3");
    let cwd_arg = cwd.display().to_string();
    command.args([
        "-m",
        "smarty_agents.cli",
        "codex",
        "setup",
        "--root",
        &cwd_arg,
        "--json",
    ]);
    command
}

fn pythonpath_with_root(root: &Path) -> String {
    let src = root.join("src").display().to_string();
    match std::env::var("PYTHONPATH") {
        Ok(existing) if !existing.is_empty() => format!("{src}:{existing}"),
        _ => src,
    }
}

fn format_setup_output(success: bool, stdout: &[u8], stderr: &[u8]) -> String {
    let title = if success {
        "Smarty setup completed."
    } else {
        "Smarty setup command failed."
    };
    let out = String::from_utf8_lossy(stdout).trim().to_string();
    let err = String::from_utf8_lossy(stderr).trim().to_string();
    match (out.is_empty(), err.is_empty()) {
        (false, false) => format!("{title}\n{out}\n{err}"),
        (false, true) => format!("{title}\n{out}"),
        (true, false) => format!("{title}\n{err}"),
        (true, true) => title.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::format_setup_output;

    #[test]
    fn formats_success_output() {
        let text = format_setup_output(true, br#"{"stage":"completed"}"#, b"");
        assert!(text.contains("Smarty setup completed."));
        assert!(text.contains("\"completed\""));
    }

    #[test]
    fn formats_failure_output() {
        let text = format_setup_output(false, b"", b"missing smarty-agents");
        assert!(text.contains("Smarty setup command failed."));
        assert!(text.contains("missing smarty-agents"));
    }
}

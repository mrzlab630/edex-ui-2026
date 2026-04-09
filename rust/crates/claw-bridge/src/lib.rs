use core_domain::{AgentPermissionMode, AgentTask};
use std::path::{Path, PathBuf};
use std::process::Command;

const DEFAULT_CLAW_ENV: &str = "EDEX_CORE_CLAW_BIN";
const DEFAULT_CARGO_CLAW_PATH: &str = ".cargo/bin/claw";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClawProvider {
    binary_path: PathBuf,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClawStatusReport {
    pub binary_path: PathBuf,
    pub version: String,
    pub doctor: String,
    pub status: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClawTaskResult {
    pub output: String,
}

#[derive(Debug, thiserror::Error)]
pub enum ClawError {
    #[error("claw binary is not configured")]
    BinaryNotConfigured,
    #[error("claw binary does not exist at `{0}`")]
    BinaryMissing(PathBuf),
    #[error("claw command `{action}` failed: {message}")]
    CommandFailed {
        action: &'static str,
        message: String,
    },
    #[error("claw command `{action}` exited with status {status}: {stderr}")]
    ExitFailure {
        action: &'static str,
        status: i32,
        stderr: String,
    },
}

impl ClawProvider {
    pub fn new(binary_path: impl Into<PathBuf>) -> Result<Self, ClawError> {
        let binary_path = binary_path.into();
        if !binary_path.exists() {
            return Err(ClawError::BinaryMissing(binary_path));
        }

        Ok(Self { binary_path })
    }

    pub fn discover() -> Result<Option<Self>, ClawError> {
        if let Ok(path) = std::env::var(DEFAULT_CLAW_ENV) {
            return Self::new(path).map(Some);
        }

        if let Some(home) = std::env::var_os("HOME") {
            let candidate = PathBuf::from(home).join(DEFAULT_CARGO_CLAW_PATH);
            if candidate.exists() {
                return Self::new(candidate).map(Some);
            }
        }

        for entry in std::env::split_paths(&std::env::var_os("PATH").unwrap_or_default()) {
            let candidate = entry.join("claw");
            if candidate.exists() {
                return Self::new(candidate).map(Some);
            }
        }

        Ok(None)
    }

    pub fn binary_path(&self) -> &Path {
        &self.binary_path
    }

    pub fn probe(&self) -> Result<ClawStatusReport, ClawError> {
        Ok(ClawStatusReport {
            binary_path: self.binary_path.clone(),
            version: self.run_capture("version", &[])?,
            doctor: self.run_capture("doctor", &[])?,
            status: self.run_capture("status", &[])?,
        })
    }

    pub fn run_task(&self, task: &AgentTask, prompt: &str) -> Result<ClawTaskResult, ClawError> {
        let mut command = Command::new(&self.binary_path);
        command
            .current_dir(&task.cwd)
            .arg("--permission-mode")
            .arg(permission_mode_flag(task.permission_mode))
            .arg("--output-format")
            .arg("text")
            .arg("--compact");

        if let Some(model) = task.model.as_deref() {
            command.arg("--model").arg(model);
        }

        if !task.allowed_tools.is_empty() {
            command
                .arg("--allowedTools")
                .arg(task.allowed_tools.join(","));
        }

        command.arg("prompt").arg(prompt);

        let output = command.output().map_err(|error| ClawError::CommandFailed {
            action: "prompt",
            message: error.to_string(),
        })?;

        if !output.status.success() {
            return Err(ClawError::ExitFailure {
                action: "prompt",
                status: output.status.code().unwrap_or(-1),
                stderr: stderr_or_stdout(&output.stderr, &output.stdout),
            });
        }

        Ok(ClawTaskResult {
            output: String::from_utf8_lossy(&output.stdout).trim().to_owned(),
        })
    }

    fn run_capture(&self, action: &'static str, args: &[&str]) -> Result<String, ClawError> {
        let output = Command::new(&self.binary_path)
            .args(args)
            .arg(action)
            .output()
            .map_err(|error| ClawError::CommandFailed {
                action,
                message: error.to_string(),
            })?;

        if !output.status.success() {
            return Err(ClawError::ExitFailure {
                action,
                status: output.status.code().unwrap_or(-1),
                stderr: stderr_or_stdout(&output.stderr, &output.stdout),
            });
        }

        Ok(String::from_utf8_lossy(&output.stdout).trim().to_owned())
    }
}

fn permission_mode_flag(mode: AgentPermissionMode) -> &'static str {
    match mode {
        AgentPermissionMode::ReadOnly => "read-only",
        AgentPermissionMode::WorkspaceWrite => "workspace-write",
        AgentPermissionMode::DangerFullAccess => "danger-full-access",
    }
}

fn stderr_or_stdout(stderr: &[u8], stdout: &[u8]) -> String {
    let stderr = String::from_utf8_lossy(stderr).trim().to_owned();
    if !stderr.is_empty() {
        return stderr;
    }

    String::from_utf8_lossy(stdout).trim().to_owned()
}

#[cfg(test)]
mod tests {
    use super::*;
    use core_domain::{AgentTask, AgentTaskDraft};
    use std::fs;
    use uuid::Uuid;

    #[test]
    fn probe_collects_local_reports_from_binary() {
        let script_dir = test_dir("probe");
        let script_path = script_dir.join("claw");
        write_fake_claw(
            &script_path,
            r#"#!/bin/sh
case "$*" in
  *version*) printf 'Claw Code\nVersion test\n' ;;
  *doctor*) printf 'Doctor OK\n' ;;
  *status*) printf 'Status OK\n' ;;
  *) printf 'unexpected args: %s\n' "$*" >&2; exit 1 ;;
esac
"#,
        );

        let provider = ClawProvider::new(&script_path).expect("provider should initialize");
        let report = provider.probe().expect("probe should succeed");

        assert!(report.version.contains("Version test"));
        assert_eq!(report.doctor, "Doctor OK");
        assert_eq!(report.status, "Status OK");

        fs::remove_dir_all(script_dir).expect("probe temp dir should clean");
    }

    #[test]
    fn run_task_uses_task_cwd_and_arguments() {
        let script_dir = test_dir("run-task");
        let cwd = script_dir.join("workspace");
        fs::create_dir_all(&cwd).expect("cwd should exist");
        let script_path = script_dir.join("claw");
        write_fake_claw(
            &script_path,
            r#"#!/bin/sh
printf 'cwd=%s\nargs=%s\n' "$PWD" "$*"
"#,
        );

        let provider = ClawProvider::new(&script_path).expect("provider should initialize");
        let task = AgentTask::new(AgentTaskDraft {
            id: Uuid::new_v4(),
            workspace_id: Uuid::new_v4(),
            session_id: None,
            cwd: cwd.clone(),
            prompt: "summarize recent failures".into(),
            model: Some("claude-opus-4-6".into()),
            context_query: Some("cargo test".into()),
            context_limit: 4,
            permission_mode: AgentPermissionMode::WorkspaceWrite,
            allowed_tools: vec!["read".into(), "glob".into()],
        })
        .expect("task should be valid");

        let result = provider
            .run_task(&task, "rendered prompt")
            .expect("task should execute");

        assert!(result.output.contains(&format!("cwd={}", cwd.display())));
        assert!(result.output.contains("--permission-mode workspace-write"));
        assert!(result.output.contains("--allowedTools glob,read"));
        assert!(result.output.contains("prompt rendered prompt"));

        fs::remove_dir_all(script_dir).expect("run-task temp dir should clean");
    }

    fn test_dir(label: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "edex-ui-2026-claw-bridge-{label}-{}",
            Uuid::new_v4()
        ));
        fs::create_dir_all(&dir).expect("test dir should exist");
        dir
    }

    fn write_fake_claw(path: &Path, body: &str) {
        fs::write(path, body).expect("fake claw should write");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mut permissions = fs::metadata(path)
                .expect("fake claw metadata should exist")
                .permissions();
            permissions.set_mode(0o755);
            fs::set_permissions(path, permissions).expect("fake claw should be executable");
        }
    }
}

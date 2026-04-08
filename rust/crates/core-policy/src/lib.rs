use core_domain::AgentPermissionMode;
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use thiserror::Error;

pub const DEFAULT_MAX_IN_MEMORY_HISTORY_ENTRIES: usize = 2_048;
pub const DEFAULT_MAX_INDEXED_ROOTS: usize = 16;
pub const DEFAULT_MAX_INDEXED_ENTRIES_PER_ROOT: usize = 20_000;
pub const DEFAULT_MAX_FILE_PREVIEW_BYTES: usize = 64 * 1024;
pub const DEFAULT_MAX_FILE_PREVIEW_LINES: usize = 400;

const READ_ONLY_TOOLS: &[&str] = &["glob", "grep", "read"];
const WORKSPACE_WRITE_TOOLS: &[&str] = &["edit", "glob", "grep", "mkdir", "read", "write"];

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
pub enum Decision {
    Allow,
    Deny,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
pub enum Sensitivity {
    SafeMetadata,
    RedactableContent,
    SensitiveRawContent,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AgentPolicyDecision {
    pub permission_mode: AgentPermissionMode,
    pub allowed_tools: Vec<String>,
}

#[derive(Debug, Error, PartialEq, Eq)]
pub enum PolicyError {
    #[error("policy denied")]
    Denied,
    #[error("danger-full-access agent mode is disabled by core policy")]
    DangerModeDenied,
    #[error("tool `{0}` is not permitted by agent policy")]
    ToolDenied(String),
    #[error("no workspace roots are registered")]
    NoWorkspaceRoots,
    #[error("path `{path}` is outside registered workspace roots")]
    PathOutsideWorkspaceRoots { path: String },
}

pub fn enforce_agent_policy(
    requested_mode: AgentPermissionMode,
    requested_tools: &[String],
) -> Result<AgentPolicyDecision, PolicyError> {
    let permission_mode = match requested_mode {
        AgentPermissionMode::ReadOnly => AgentPermissionMode::ReadOnly,
        AgentPermissionMode::WorkspaceWrite => AgentPermissionMode::WorkspaceWrite,
        AgentPermissionMode::DangerFullAccess => return Err(PolicyError::DangerModeDenied),
    };

    let allowed_catalog = allowed_tool_catalog(permission_mode);
    let mut allowed_tools = if requested_tools.is_empty() {
        allowed_catalog.iter().map(|tool| (*tool).to_owned()).collect()
    } else {
        let mut tools = Vec::with_capacity(requested_tools.len());
        for tool in requested_tools {
            if !allowed_catalog.contains(&tool.as_str()) {
                return Err(PolicyError::ToolDenied(tool.clone()));
            }
            tools.push(tool.clone());
        }
        tools
    };

    allowed_tools.sort();
    allowed_tools.dedup();

    Ok(AgentPolicyDecision {
        permission_mode,
        allowed_tools,
    })
}

pub fn ensure_path_within_roots(path: &Path, roots: &[PathBuf]) -> Result<(), PolicyError> {
    if roots.is_empty() {
        return Err(PolicyError::NoWorkspaceRoots);
    }

    if roots.iter().any(|root| path.starts_with(root)) {
        return Ok(());
    }

    Err(PolicyError::PathOutsideWorkspaceRoots {
        path: path.display().to_string(),
    })
}

fn allowed_tool_catalog(mode: AgentPermissionMode) -> &'static [&'static str] {
    match mode {
        AgentPermissionMode::ReadOnly => READ_ONLY_TOOLS,
        AgentPermissionMode::WorkspaceWrite => WORKSPACE_WRITE_TOOLS,
        AgentPermissionMode::DangerFullAccess => &[],
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn policy_denies_danger_full_access() {
        let error = enforce_agent_policy(AgentPermissionMode::DangerFullAccess, &[])
            .expect_err("danger-full-access must be denied");

        assert_eq!(error, PolicyError::DangerModeDenied);
    }

    #[test]
    fn policy_defaults_tools_for_workspace_write() {
        let decision = enforce_agent_policy(AgentPermissionMode::WorkspaceWrite, &[])
            .expect("workspace-write should be allowed");

        assert!(decision.allowed_tools.contains(&"read".to_string()));
        assert!(decision.allowed_tools.contains(&"write".to_string()));
    }

    #[test]
    fn policy_denies_unknown_tools() {
        let error = enforce_agent_policy(
            AgentPermissionMode::ReadOnly,
            &[String::from("shell"), String::from("read")],
        )
        .expect_err("unknown tool must be denied");

        assert_eq!(error, PolicyError::ToolDenied("shell".into()));
    }

    #[test]
    fn path_policy_requires_roots() {
        let error = ensure_path_within_roots(Path::new("/tmp"), &[])
            .expect_err("missing roots must be denied");

        assert_eq!(error, PolicyError::NoWorkspaceRoots);
    }
}

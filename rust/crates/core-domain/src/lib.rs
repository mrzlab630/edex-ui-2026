use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use thiserror::Error;
use uuid::Uuid;

pub type WorkspaceId = Uuid;
pub type SessionId = Uuid;
pub type HostId = Uuid;
pub type TunnelId = Uuid;
pub type AgentTaskId = Uuid;
pub type HistoryEntryId = Uuid;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Workspace {
    pub id: WorkspaceId,
    pub name: String,
    pub roots: Vec<PathBuf>,
    pub bookmarks: Vec<PathBuf>,
}

impl Workspace {
    pub fn new(id: WorkspaceId, name: impl Into<String>) -> Result<Self, DomainError> {
        let name = name.into().trim().to_owned();
        if name.is_empty() {
            return Err(DomainError::EmptyWorkspaceName);
        }

        Ok(Self {
            id,
            name,
            roots: Vec::new(),
            bookmarks: Vec::new(),
        })
    }

    pub fn with_roots(mut self, roots: Vec<PathBuf>) -> Result<Self, DomainError> {
        self.roots = normalize_paths(roots)?;
        Ok(self)
    }

    pub fn with_bookmarks(mut self, bookmarks: Vec<PathBuf>) -> Result<Self, DomainError> {
        self.bookmarks = normalize_paths(bookmarks)?;
        Ok(self)
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
pub enum SessionKind {
    Local,
    Ssh,
    Tmux,
    Task,
    Agent,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
pub enum SessionBacking {
    LocalPty,
    TmuxSession,
    RemoteTmux,
    RemoteShell,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Session {
    pub id: SessionId,
    pub workspace_id: WorkspaceId,
    pub kind: SessionKind,
    pub backing: SessionBacking,
    pub cwd: PathBuf,
}

impl Session {
    pub fn new(
        id: SessionId,
        workspace_id: WorkspaceId,
        kind: SessionKind,
        backing: SessionBacking,
        cwd: impl Into<PathBuf>,
    ) -> Result<Self, DomainError> {
        let cwd = cwd.into();
        if cwd.as_os_str().is_empty() {
            return Err(DomainError::EmptySessionCwd);
        }

        validate_session_backing(kind, backing)?;

        Ok(Self {
            id,
            workspace_id,
            kind,
            backing,
            cwd,
        })
    }
}

fn validate_session_backing(kind: SessionKind, backing: SessionBacking) -> Result<(), DomainError> {
    let supported = matches!(
        (kind, backing),
        (SessionKind::Local, SessionBacking::LocalPty)
            | (SessionKind::Ssh, SessionBacking::RemoteShell)
            | (SessionKind::Tmux, SessionBacking::TmuxSession)
            | (SessionKind::Tmux, SessionBacking::RemoteTmux)
    );

    if supported {
        Ok(())
    } else {
        Err(DomainError::UnsupportedSessionBacking { kind, backing })
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct HostProfile {
    pub id: HostId,
    pub display_name: String,
    pub ssh_target: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct TunnelProfile {
    pub id: TunnelId,
    pub host_id: HostId,
    pub bind_address: String,
    pub bind_port: u16,
    pub target_host: String,
    pub target_port: u16,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
pub enum AgentProviderKind {
    Claw,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum AgentPermissionMode {
    ReadOnly,
    WorkspaceWrite,
    DangerFullAccess,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct AgentTask {
    pub id: AgentTaskId,
    pub workspace_id: WorkspaceId,
    pub provider: AgentProviderKind,
    pub cwd: PathBuf,
    pub prompt: String,
    pub model: Option<String>,
    pub context_query: Option<String>,
    pub context_limit: usize,
    pub permission_mode: AgentPermissionMode,
    pub allowed_tools: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct AgentTaskDraft {
    pub id: AgentTaskId,
    pub workspace_id: WorkspaceId,
    pub cwd: PathBuf,
    pub prompt: String,
    pub model: Option<String>,
    pub context_query: Option<String>,
    pub context_limit: usize,
    pub permission_mode: AgentPermissionMode,
    pub allowed_tools: Vec<String>,
}

impl AgentTask {
    pub fn new(draft: AgentTaskDraft) -> Result<Self, DomainError> {
        let cwd = draft.cwd;
        if cwd.as_os_str().is_empty() {
            return Err(DomainError::EmptyAgentCwd);
        }

        let prompt = draft.prompt.trim().to_owned();
        if prompt.is_empty() {
            return Err(DomainError::EmptyAgentPrompt);
        }

        if draft.context_limit == 0 {
            return Err(DomainError::InvalidContextLimit);
        }

        let context_query = normalize_optional_text(draft.context_query);
        let model = normalize_optional_text(draft.model);
        let allowed_tools = normalize_tools(draft.allowed_tools)?;

        Ok(Self {
            id: draft.id,
            workspace_id: draft.workspace_id,
            provider: AgentProviderKind::Claw,
            cwd,
            prompt,
            model,
            context_query,
            context_limit: draft.context_limit,
            permission_mode: draft.permission_mode,
            allowed_tools,
        })
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
pub enum HistoryEntryKind {
    TerminalInput,
    TerminalOutput,
    ChatUser,
    ChatAgent,
    SystemEvent,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct HistoryEntry {
    pub id: HistoryEntryId,
    pub workspace_id: WorkspaceId,
    pub session_id: Option<SessionId>,
    pub kind: HistoryEntryKind,
    pub at_unix_ms: u64,
    pub content: String,
}

impl HistoryEntry {
    pub fn new(
        id: HistoryEntryId,
        workspace_id: WorkspaceId,
        session_id: Option<SessionId>,
        kind: HistoryEntryKind,
        at_unix_ms: u64,
        content: impl Into<String>,
    ) -> Result<Self, DomainError> {
        let content = content.into().trim().to_owned();
        if content.is_empty() {
            return Err(DomainError::EmptyHistoryContent);
        }

        Ok(Self {
            id,
            workspace_id,
            session_id,
            kind,
            at_unix_ms,
            content,
        })
    }
}

#[derive(Debug, Error, PartialEq, Eq)]
pub enum DomainError {
    #[error("workspace name cannot be empty")]
    EmptyWorkspaceName,
    #[error("workspace path cannot be empty")]
    EmptyWorkspacePath,
    #[error("session cwd cannot be empty")]
    EmptySessionCwd,
    #[error("agent cwd cannot be empty")]
    EmptyAgentCwd,
    #[error("agent prompt cannot be empty")]
    EmptyAgentPrompt,
    #[error("agent context limit must be greater than zero")]
    InvalidContextLimit,
    #[error("agent allowed tool names must be non-empty")]
    EmptyAllowedTool,
    #[error("history content cannot be empty")]
    EmptyHistoryContent,
    #[error("session kind `{kind:?}` does not support backing `{backing:?}`")]
    UnsupportedSessionBacking {
        kind: SessionKind,
        backing: SessionBacking,
    },
    #[error("invalid state transition")]
    InvalidStateTransition,
}

fn normalize_optional_text(value: Option<String>) -> Option<String> {
    value
        .map(|value| value.trim().to_owned())
        .filter(|value| !value.is_empty())
}

fn normalize_paths(paths: Vec<PathBuf>) -> Result<Vec<PathBuf>, DomainError> {
    let mut normalized = Vec::with_capacity(paths.len());
    for path in paths {
        if path.as_os_str().is_empty() {
            return Err(DomainError::EmptyWorkspacePath);
        }
        normalized.push(path);
    }
    normalized.sort();
    normalized.dedup();
    Ok(normalized)
}

fn normalize_tools(allowed_tools: Vec<String>) -> Result<Vec<String>, DomainError> {
    let mut normalized = Vec::with_capacity(allowed_tools.len());
    for tool in allowed_tools {
        let tool = tool.trim().to_owned();
        if tool.is_empty() {
            return Err(DomainError::EmptyAllowedTool);
        }
        normalized.push(tool);
    }
    normalized.sort();
    normalized.dedup();
    Ok(normalized)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn workspace_requires_non_blank_name() {
        let error = Workspace::new(Uuid::new_v4(), "  ").expect_err("blank names must fail");
        assert_eq!(error, DomainError::EmptyWorkspaceName);
    }

    #[test]
    fn workspace_rejects_empty_root_path() {
        let error = Workspace::new(Uuid::new_v4(), "main")
            .and_then(|workspace| workspace.with_roots(vec![PathBuf::new()]))
            .expect_err("empty root path must fail");

        assert_eq!(error, DomainError::EmptyWorkspacePath);
    }

    #[test]
    fn session_requires_non_empty_cwd() {
        let error = Session::new(
            Uuid::new_v4(),
            Uuid::new_v4(),
            SessionKind::Local,
            SessionBacking::LocalPty,
            PathBuf::new(),
        )
        .expect_err("empty cwd must fail");

        assert_eq!(error, DomainError::EmptySessionCwd);
    }

    #[test]
    fn session_rejects_unsupported_kind_backing_pair() {
        let error = Session::new(
            Uuid::new_v4(),
            Uuid::new_v4(),
            SessionKind::Agent,
            SessionBacking::RemoteTmux,
            "/workspace",
        )
        .expect_err("unsupported session pair must fail");

        assert_eq!(
            error,
            DomainError::UnsupportedSessionBacking {
                kind: SessionKind::Agent,
                backing: SessionBacking::RemoteTmux,
            }
        );
    }

    #[test]
    fn history_entry_requires_non_empty_content() {
        let error = HistoryEntry::new(
            Uuid::new_v4(),
            Uuid::new_v4(),
            None,
            HistoryEntryKind::SystemEvent,
            1,
            "   ",
        )
        .expect_err("blank history content must fail");

        assert_eq!(error, DomainError::EmptyHistoryContent);
    }

    #[test]
    fn agent_task_requires_non_empty_prompt() {
        let error = AgentTask::new(AgentTaskDraft {
            id: Uuid::new_v4(),
            workspace_id: Uuid::new_v4(),
            cwd: "/workspace".into(),
            prompt: "   ".into(),
            model: None,
            context_query: None,
            context_limit: 4,
            permission_mode: AgentPermissionMode::WorkspaceWrite,
            allowed_tools: Vec::new(),
        })
        .expect_err("blank prompt must fail");

        assert_eq!(error, DomainError::EmptyAgentPrompt);
    }

    #[test]
    fn agent_task_rejects_zero_context_limit() {
        let error = AgentTask::new(AgentTaskDraft {
            id: Uuid::new_v4(),
            workspace_id: Uuid::new_v4(),
            cwd: "/workspace".into(),
            prompt: "summarize recent failures".into(),
            model: None,
            context_query: None,
            context_limit: 0,
            permission_mode: AgentPermissionMode::WorkspaceWrite,
            allowed_tools: Vec::new(),
        })
        .expect_err("zero context limit must fail");

        assert_eq!(error, DomainError::InvalidContextLimit);
    }

    #[test]
    fn agent_task_normalizes_optional_fields_and_tools() {
        let task = AgentTask::new(AgentTaskDraft {
            id: Uuid::new_v4(),
            workspace_id: Uuid::new_v4(),
            cwd: "/workspace".into(),
            prompt: "summarize recent failures".into(),
            model: Some(" claude-opus-4-6 ".into()),
            context_query: Some(" cargo test ".into()),
            context_limit: 5,
            permission_mode: AgentPermissionMode::WorkspaceWrite,
            allowed_tools: vec![" read ".into(), "glob".into(), "read".into()],
        })
        .expect("task should be valid");

        assert_eq!(task.model.as_deref(), Some("claude-opus-4-6"));
        assert_eq!(task.context_query.as_deref(), Some("cargo test"));
        assert_eq!(
            task.allowed_tools,
            vec!["glob".to_string(), "read".to_string()]
        );
    }
}

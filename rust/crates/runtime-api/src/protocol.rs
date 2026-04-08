use context_engine::ContextResult;
use core_domain::{
    AgentPermissionMode, AgentTask, HistoryEntry, HistoryEntryKind, Session, SessionBacking,
    SessionKind,
};
use core_events::CoreEvent;
use file_index::{FileEntry, FilePreview, FileSearchResult};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use uuid::Uuid;

use crate::framing::FrameAssumptions;

pub const API_VERSION: &str = "0.1.0-dev";

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum RequestKind {
    Command,
    Query,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct AgentProviderStatus {
    pub provider: String,
    pub available: bool,
    pub binary_path: Option<String>,
    pub version_report: Option<String>,
    pub doctor_report: Option<String>,
    pub status_report: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct SshTcpForward {
    pub bind_address: String,
    pub bind_port: u16,
    pub target_host: String,
    pub target_port: u16,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct SshDynamicForward {
    pub bind_address: String,
    pub bind_port: u16,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct SshHostProfile {
    pub alias: String,
    pub hostname: String,
    pub user: Option<String>,
    pub port: Option<u16>,
    pub identity_file: Option<String>,
    pub proxy_jump: Option<String>,
    pub local_forwards: Vec<SshTcpForward>,
    pub remote_forwards: Vec<SshTcpForward>,
    pub dynamic_forwards: Vec<SshDynamicForward>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum Command {
    Ping,
    RegisterWorkspace {
        workspace_id: Uuid,
        name: String,
        roots: Vec<String>,
    },
    AppendHistoryEntry {
        entry_id: Uuid,
        workspace_id: Uuid,
        session_id: Option<Uuid>,
        kind: HistoryEntryKind,
        at_unix_ms: u64,
        content: String,
    },
    RunAgentTask {
        task_id: Uuid,
        workspace_id: Uuid,
        cwd: Option<String>,
        prompt: String,
        model: Option<String>,
        context_query: Option<String>,
        context_limit: usize,
        permission_mode: AgentPermissionMode,
        allowed_tools: Vec<String>,
    },
    ImportSshConfig {
        config_text: String,
    },
    RefreshFileIndex {
        workspace_id: Uuid,
        root_path: String,
    },
    ExportRecoveryBundle {
        bundle_path: String,
    },
    ImportRecoveryBundle {
        bundle_path: String,
    },
    RekeyRecoveryBundle {
        input_path: String,
        output_path: String,
        new_passphrase: String,
    },
    RegisterLocalTmuxSession {
        session_id: Uuid,
        workspace_id: Uuid,
        cwd: String,
    },
    RegisterSession {
        session_id: Uuid,
        workspace_id: Uuid,
        kind: SessionKind,
        backing: SessionBacking,
        cwd: String,
    },
    RemoveSession {
        session_id: Uuid,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum Query {
    Health,
    AgentProviderStatus,
    WorkspaceCount,
    ContextSearch {
        workspace_id: Option<Uuid>,
        session_id: Option<Uuid>,
        text: String,
        limit: usize,
    },
    FileList {
        workspace_id: Uuid,
        path: String,
        limit: usize,
    },
    FileStat {
        workspace_id: Uuid,
        path: String,
    },
    FilePreview {
        workspace_id: Uuid,
        path: String,
        max_bytes: usize,
        max_lines: usize,
    },
    FileSearch {
        workspace_id: Uuid,
        root_path: String,
        text: String,
        limit: usize,
    },
    RecentHistory {
        workspace_id: Option<Uuid>,
        session_id: Option<Uuid>,
        limit: usize,
    },
    SshHosts,
    Sessions {
        workspace_id: Option<Uuid>,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct EventEnvelope {
    pub event: CoreEvent,
    pub metadata: Value,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum RequestPayload {
    Command(Command),
    Query(Query),
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct RequestEnvelope {
    pub request_id: String,
    pub payload: RequestPayload,
}

impl RequestEnvelope {
    pub fn new(request_id: impl Into<String>, payload: RequestPayload) -> Self {
        Self {
            request_id: request_id.into(),
            payload,
        }
    }

    pub fn command(request_id: impl Into<String>, command: Command) -> Self {
        Self::new(request_id, RequestPayload::Command(command))
    }

    pub fn query(request_id: impl Into<String>, query: Query) -> Self {
        Self::new(request_id, RequestPayload::Query(query))
    }

    pub fn kind(&self) -> RequestKind {
        match self.payload {
            RequestPayload::Command(_) => RequestKind::Command,
            RequestPayload::Query(_) => RequestKind::Query,
        }
    }

    pub fn framing(&self) -> FrameAssumptions {
        FrameAssumptions::local_json_over_uds()
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum RuntimeStatus {
    Starting,
    Ready,
    Degraded,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct DaemonInfo {
    pub socket_path: String,
    pub api_version: String,
    pub schema_version: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct HealthSnapshot {
    pub status: RuntimeStatus,
    pub daemon: DaemonInfo,
    pub workspace_count: usize,
    pub session_count: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ErrorCode {
    InvalidRequest,
    ValidationFailed,
    UnsupportedVersion,
    NotFound,
    Busy,
    Internal,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ApiError {
    pub code: ErrorCode,
    pub message: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub details: Option<Value>,
}

impl ApiError {
    pub fn new(code: ErrorCode, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
            details: None,
        }
    }

    pub fn with_details(mut self, details: Value) -> Self {
        self.details = Some(details);
        self
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ResponsePayload {
    Pong,
    Health(HealthSnapshot),
    AgentProviderStatus {
        status: AgentProviderStatus,
    },
    WorkspaceCount {
        workspace_count: usize,
    },
    AgentTaskCompleted {
        task: AgentTask,
        output: String,
        context_result_count: usize,
        history_count: usize,
    },
    WorkspaceRegistered {
        workspace_count: usize,
    },
    HistoryAppended {
        history_count: usize,
    },
    HistoryEntries {
        entries: Vec<HistoryEntry>,
    },
    ContextResults {
        results: Vec<ContextResult>,
    },
    FileIndexRefreshed {
        root_path: String,
        indexed_count: usize,
    },
    FileEntries {
        entries: Vec<FileEntry>,
    },
    FileMetadata {
        entry: FileEntry,
    },
    FilePreview {
        preview: FilePreview,
    },
    FileSearchResults {
        results: Vec<FileSearchResult>,
    },
    SshConfigImported {
        host_count: usize,
    },
    RecoveryBundleExported {
        bundle_path: String,
        workspace_count: usize,
        session_count: usize,
        history_count: usize,
        ssh_host_count: usize,
    },
    RecoveryBundleImported {
        bundle_path: String,
        workspace_count: usize,
        session_count: usize,
        history_count: usize,
        ssh_host_count: usize,
    },
    RecoveryBundleRekeyed {
        output_path: String,
        workspace_count: usize,
        session_count: usize,
        history_count: usize,
        ssh_host_count: usize,
    },
    SshHosts {
        hosts: Vec<SshHostProfile>,
    },
    SessionRegistered {
        session: Session,
        session_count: usize,
    },
    SessionRemoved {
        session_id: Uuid,
        session_count: usize,
    },
    Sessions {
        sessions: Vec<Session>,
    },
    Error(ApiError),
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ResponseEnvelope {
    pub request_id: String,
    pub ok: bool,
    pub payload: ResponsePayload,
}

impl ResponseEnvelope {
    pub fn ok(request_id: String, payload: ResponsePayload) -> Self {
        Self {
            request_id,
            ok: true,
            payload,
        }
    }

    pub fn error(request_id: impl Into<String>, error: ApiError) -> Self {
        Self {
            request_id: request_id.into(),
            ok: false,
            payload: ResponsePayload::Error(error),
        }
    }

    pub fn is_error(&self) -> bool {
        !self.ok
    }

    pub fn framing(&self) -> FrameAssumptions {
        FrameAssumptions::local_json_over_uds()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{decode_json_frame, encode_json_frame, FrameEncoding};

    #[test]
    fn request_helpers_keep_protocol_small_and_typed() {
        let request = RequestEnvelope::query("req-health", Query::Health);

        assert_eq!(request.kind(), RequestKind::Query);

        let framing = request.framing();
        assert_eq!(framing.encoding, FrameEncoding::JsonLines);
        assert_eq!(framing.max_frame_bytes, crate::MAX_FRAME_BYTES);
    }

    #[test]
    fn error_response_roundtrips_over_json_frame() {
        let envelope = ResponseEnvelope::error(
            "req-42",
            ApiError::new(ErrorCode::ValidationFailed, "workspace name is required"),
        );

        let encoded = encode_json_frame(&envelope).expect("frame encodes");
        let decoded: ResponseEnvelope =
            decode_json_frame(std::str::from_utf8(&encoded).expect("utf8"))
                .expect("frame decodes")
                .expect("frame contains payload");

        assert!(decoded.is_error());
        assert_eq!(decoded.request_id, "req-42");
        assert!(matches!(
            decoded.payload,
            ResponsePayload::Error(ApiError {
                code: ErrorCode::ValidationFailed,
                ..
            })
        ));
    }

    #[test]
    fn session_payload_roundtrips_with_domain_types() {
        let session = Session::new(
            Uuid::new_v4(),
            Uuid::new_v4(),
            SessionKind::Local,
            SessionBacking::LocalPty,
            "/workspace",
        )
        .expect("session should be valid");

        let envelope = ResponseEnvelope::ok(
            "req-sessions".into(),
            ResponsePayload::Sessions {
                sessions: vec![session.clone()],
            },
        );

        let encoded = encode_json_frame(&envelope).expect("frame encodes");
        let decoded: ResponseEnvelope =
            decode_json_frame(std::str::from_utf8(&encoded).expect("utf8"))
                .expect("frame decodes")
                .expect("frame contains payload");

        assert_eq!(
            decoded.payload,
            ResponsePayload::Sessions {
                sessions: vec![session]
            }
        );
    }

    #[test]
    fn local_tmux_command_roundtrips() {
        let request = RequestEnvelope::command(
            "req-local-tmux",
            Command::RegisterLocalTmuxSession {
                session_id: Uuid::new_v4(),
                workspace_id: Uuid::new_v4(),
                cwd: "/workspace".into(),
            },
        );

        let encoded = encode_json_frame(&request).expect("frame encodes");
        let decoded: RequestEnvelope =
            decode_json_frame(std::str::from_utf8(&encoded).expect("utf8"))
                .expect("frame decodes")
                .expect("frame contains payload");

        assert!(matches!(
            decoded.payload,
            RequestPayload::Command(Command::RegisterLocalTmuxSession { .. })
        ));
    }

    #[test]
    fn history_commands_roundtrip() {
        let append = RequestEnvelope::command(
            "req-history-append",
            Command::AppendHistoryEntry {
                entry_id: Uuid::new_v4(),
                workspace_id: Uuid::new_v4(),
                session_id: Some(Uuid::new_v4()),
                kind: HistoryEntryKind::TerminalOutput,
                at_unix_ms: 42,
                content: "cargo test --workspace".into(),
            },
        );
        let encoded = encode_json_frame(&append).expect("frame encodes");
        let decoded: RequestEnvelope =
            decode_json_frame(std::str::from_utf8(&encoded).expect("utf8"))
                .expect("frame decodes")
                .expect("frame contains payload");
        assert!(matches!(
            decoded.payload,
            RequestPayload::Command(Command::AppendHistoryEntry { .. })
        ));

        let query = RequestEnvelope::query(
            "req-history-recent",
            Query::RecentHistory {
                workspace_id: None,
                session_id: None,
                limit: 25,
            },
        );
        let encoded = encode_json_frame(&query).expect("frame encodes");
        let decoded: RequestEnvelope =
            decode_json_frame(std::str::from_utf8(&encoded).expect("utf8"))
                .expect("frame decodes")
                .expect("frame contains payload");
        assert!(matches!(
            decoded.payload,
            RequestPayload::Query(Query::RecentHistory { limit: 25, .. })
        ));

        let context_query = RequestEnvelope::query(
            "req-context-search",
            Query::ContextSearch {
                workspace_id: None,
                session_id: None,
                text: "cargo test".into(),
                limit: 5,
            },
        );
        let encoded = encode_json_frame(&context_query).expect("frame encodes");
        let decoded: RequestEnvelope =
            decode_json_frame(std::str::from_utf8(&encoded).expect("utf8"))
                .expect("frame decodes")
                .expect("frame contains payload");
        assert!(matches!(
            decoded.payload,
            RequestPayload::Query(Query::ContextSearch { limit: 5, .. })
        ));
    }

    #[test]
    fn agent_commands_and_queries_roundtrip() {
        let command = RequestEnvelope::command(
            "req-agent-run",
            Command::RunAgentTask {
                task_id: Uuid::new_v4(),
                workspace_id: Uuid::new_v4(),
                cwd: Some("/workspace".into()),
                prompt: "summarize recent failures".into(),
                model: Some("claude-opus-4-6".into()),
                context_query: Some("cargo test".into()),
                context_limit: 5,
                permission_mode: AgentPermissionMode::WorkspaceWrite,
                allowed_tools: vec!["read".into(), "glob".into()],
            },
        );

        let encoded = encode_json_frame(&command).expect("frame encodes");
        let decoded: RequestEnvelope =
            decode_json_frame(std::str::from_utf8(&encoded).expect("utf8"))
                .expect("frame decodes")
                .expect("frame contains payload");
        assert!(matches!(
            decoded.payload,
            RequestPayload::Command(Command::RunAgentTask {
                context_limit: 5,
                ..
            })
        ));

        let query = RequestEnvelope::query("req-agent-status", Query::AgentProviderStatus);
        let encoded = encode_json_frame(&query).expect("frame encodes");
        let decoded: RequestEnvelope =
            decode_json_frame(std::str::from_utf8(&encoded).expect("utf8"))
                .expect("frame decodes")
                .expect("frame contains payload");
        assert!(matches!(
            decoded.payload,
            RequestPayload::Query(Query::AgentProviderStatus)
        ));
    }

    #[test]
    fn ssh_import_command_roundtrips() {
        let request = RequestEnvelope::command(
            "req-ssh-import",
            Command::ImportSshConfig {
                config_text: "Host alpha\n  HostName alpha.example\n".into(),
            },
        );

        let encoded = encode_json_frame(&request).expect("frame encodes");
        let decoded: RequestEnvelope =
            decode_json_frame(std::str::from_utf8(&encoded).expect("utf8"))
                .expect("frame decodes")
                .expect("frame contains payload");

        assert!(matches!(
            decoded.payload,
            RequestPayload::Command(Command::ImportSshConfig { .. })
        ));
    }

    #[test]
    fn recovery_commands_roundtrip() {
        for command in [
            Command::RefreshFileIndex {
                workspace_id: Uuid::new_v4(),
                root_path: "/tmp".into(),
            },
            Command::ExportRecoveryBundle {
                bundle_path: "/tmp/export.edex-recovery".into(),
            },
            Command::ImportRecoveryBundle {
                bundle_path: "/tmp/import.edex-recovery".into(),
            },
            Command::RekeyRecoveryBundle {
                input_path: "/tmp/input.edex-recovery".into(),
                output_path: "/tmp/output.edex-recovery".into(),
                new_passphrase: "next-passphrase".into(),
            },
        ] {
            let request = RequestEnvelope::command("req-recovery", command);
            let encoded = encode_json_frame(&request).expect("frame encodes");
            let decoded: RequestEnvelope =
                decode_json_frame(std::str::from_utf8(&encoded).expect("utf8"))
                    .expect("frame decodes")
                    .expect("frame contains payload");

            assert!(matches!(decoded.payload, RequestPayload::Command(_)));
        }
    }

    #[test]
    fn recovery_payload_roundtrips() {
        let envelope = ResponseEnvelope::ok(
            "req-recovery".into(),
            ResponsePayload::RecoveryBundleExported {
                bundle_path: "/tmp/export.edex-recovery".into(),
                workspace_count: 1,
                session_count: 1,
                history_count: 2,
                ssh_host_count: 1,
            },
        );

        let encoded = encode_json_frame(&envelope).expect("frame encodes");
        let decoded: ResponseEnvelope =
            decode_json_frame(std::str::from_utf8(&encoded).expect("utf8"))
                .expect("frame decodes")
                .expect("frame contains payload");

        assert!(matches!(
            decoded.payload,
            ResponsePayload::RecoveryBundleExported { .. }
        ));
    }

    #[test]
    fn file_queries_roundtrip() {
        for query in [
            Query::FileList {
                workspace_id: Uuid::new_v4(),
                path: "/tmp".into(),
                limit: 10,
            },
            Query::FileStat {
                workspace_id: Uuid::new_v4(),
                path: "/tmp/README.md".into(),
            },
            Query::FilePreview {
                workspace_id: Uuid::new_v4(),
                path: "/tmp/README.md".into(),
                max_bytes: 512,
                max_lines: 20,
            },
            Query::FileSearch {
                workspace_id: Uuid::new_v4(),
                root_path: "/tmp".into(),
                text: "cargo".into(),
                limit: 5,
            },
        ] {
            let request = RequestEnvelope::query("req-file-query", query);
            let encoded = encode_json_frame(&request).expect("frame encodes");
            let decoded: RequestEnvelope =
                decode_json_frame(std::str::from_utf8(&encoded).expect("utf8"))
                    .expect("frame decodes")
                    .expect("frame contains payload");

            assert!(matches!(decoded.payload, RequestPayload::Query(_)));
        }
    }

    #[test]
    fn file_payload_roundtrips() {
        let envelope = ResponseEnvelope::ok(
            "req-file".into(),
            ResponsePayload::FileSearchResults {
                results: vec![FileSearchResult {
                    entry: FileEntry {
                        path: "/tmp/README.md".into(),
                        name: "README.md".into(),
                        kind: file_index::FileKind::File,
                        size_bytes: Some(12),
                        modified_unix_ms: Some(1),
                        hidden: false,
                    },
                    score: 2,
                }],
            },
        );

        let encoded = encode_json_frame(&envelope).expect("frame encodes");
        let decoded: ResponseEnvelope =
            decode_json_frame(std::str::from_utf8(&encoded).expect("utf8"))
                .expect("frame decodes")
                .expect("frame contains payload");

        assert!(matches!(
            decoded.payload,
            ResponsePayload::FileSearchResults { .. }
        ));
    }

    #[test]
    fn ssh_host_payload_roundtrips() {
        let envelope = ResponseEnvelope::ok(
            "req-ssh-hosts".into(),
            ResponsePayload::SshHosts {
                hosts: vec![SshHostProfile {
                    alias: "alpha".into(),
                    hostname: "alpha.example".into(),
                    user: Some("dev".into()),
                    port: Some(2222),
                    identity_file: Some("/keys/dev".into()),
                    proxy_jump: Some("bastion".into()),
                    local_forwards: vec![SshTcpForward {
                        bind_address: "127.0.0.1".into(),
                        bind_port: 15432,
                        target_host: "db.internal".into(),
                        target_port: 5432,
                    }],
                    remote_forwards: Vec::new(),
                    dynamic_forwards: vec![SshDynamicForward {
                        bind_address: "127.0.0.1".into(),
                        bind_port: 1080,
                    }],
                }],
            },
        );

        let encoded = encode_json_frame(&envelope).expect("frame encodes");
        let decoded: ResponseEnvelope =
            decode_json_frame(std::str::from_utf8(&encoded).expect("utf8"))
                .expect("frame decodes")
                .expect("frame contains payload");

        assert!(matches!(decoded.payload, ResponsePayload::SshHosts { .. }));
    }

    #[test]
    fn history_payload_roundtrips() {
        let entry = HistoryEntry::new(
            Uuid::new_v4(),
            Uuid::new_v4(),
            Some(Uuid::new_v4()),
            HistoryEntryKind::ChatUser,
            7,
            "explain the failure path",
        )
        .expect("entry should be valid");

        let envelope = ResponseEnvelope::ok(
            "req-history".into(),
            ResponsePayload::HistoryEntries {
                entries: vec![entry],
            },
        );

        let encoded = encode_json_frame(&envelope).expect("frame encodes");
        let decoded: ResponseEnvelope =
            decode_json_frame(std::str::from_utf8(&encoded).expect("utf8"))
                .expect("frame decodes")
                .expect("frame contains payload");

        assert!(matches!(
            decoded.payload,
            ResponsePayload::HistoryEntries { .. }
        ));
    }

    #[test]
    fn context_payload_roundtrips() {
        let envelope = ResponseEnvelope::ok(
            "req-context".into(),
            ResponsePayload::ContextResults {
                results: vec![ContextResult {
                    history_entry_id: Uuid::new_v4(),
                    workspace_id: Uuid::new_v4(),
                    session_id: Some(Uuid::new_v4()),
                    kind: HistoryEntryKind::ChatAgent,
                    at_unix_ms: 9,
                    score: 3,
                    preview: "cargo test failed with sqlite error".into(),
                }],
            },
        );

        let encoded = encode_json_frame(&envelope).expect("frame encodes");
        let decoded: ResponseEnvelope =
            decode_json_frame(std::str::from_utf8(&encoded).expect("utf8"))
                .expect("frame decodes")
                .expect("frame contains payload");

        assert!(matches!(
            decoded.payload,
            ResponsePayload::ContextResults { .. }
        ));
    }

    #[test]
    fn agent_provider_status_payload_roundtrips() {
        let envelope = ResponseEnvelope::ok(
            "req-agent-status".into(),
            ResponsePayload::AgentProviderStatus {
                status: AgentProviderStatus {
                    provider: "claw".into(),
                    available: true,
                    binary_path: Some("/home/mrz/.cargo/bin/claw".into()),
                    version_report: Some("Claw Code".into()),
                    doctor_report: Some("Doctor OK".into()),
                    status_report: Some("Status OK".into()),
                },
            },
        );

        let encoded = encode_json_frame(&envelope).expect("frame encodes");
        let decoded: ResponseEnvelope =
            decode_json_frame(std::str::from_utf8(&encoded).expect("utf8"))
                .expect("frame decodes")
                .expect("frame contains payload");

        assert!(matches!(
            decoded.payload,
            ResponsePayload::AgentProviderStatus { .. }
        ));
    }
}

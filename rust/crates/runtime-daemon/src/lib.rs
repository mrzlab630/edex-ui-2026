use anyhow::{Context, Result};
use claw_bridge::{ClawError, ClawProvider, ClawStatusReport};
use context_engine::{ContextEngine, ContextQuery, ContextResult};
use core_domain::{
    AgentPermissionMode, AgentTask, AgentTaskDraft, HistoryEntry, HistoryEntryKind, Session,
};
use core_policy::{
    ensure_path_within_roots, enforce_agent_policy, AgentPolicyDecision, PolicyError,
    DEFAULT_MAX_FILE_PREVIEW_BYTES, DEFAULT_MAX_FILE_PREVIEW_LINES,
    DEFAULT_MAX_INDEXED_ENTRIES_PER_ROOT, DEFAULT_MAX_INDEXED_ROOTS,
    DEFAULT_MAX_IN_MEMORY_HISTORY_ENTRIES,
};
use core_state::{CanonicalStore, SqliteStateStore, StateError};
use file_index::{FileEntry, FileIndex, FileIndexError, FilePreview, FileSearchResult};
use history_store::{HistoryStore, HistoryStoreError};
use recovery_manager::{
    export_bundle, import_bundle, rekey_bundle, RecoveryBundle, RecoveryBundleSummary,
    RecoveryError,
};
use runtime_api::{
    decode_json_frame, encode_json_frame, ApiError, Command, DaemonInfo, ErrorCode, FrameError,
    HealthSnapshot, Query, RequestEnvelope, ResponseEnvelope, ResponsePayload, RuntimeStatus,
    SshDynamicForward, SshHostProfile, SshTcpForward, WorkspaceSummary,
};
use secrets_store::SecretsStore;
use session_broker::{
    commit_session_registration, list_sessions, plan_local_tmux_session,
    preflight_session_registration, register_session, remove_session, RegisterSessionRequest,
    SessionBrokerError, SessionFilter,
};
use ssh_bridge::{parse_ssh_config, SshError, SshHostSpec};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};
use tmux_bridge::TmuxRuntime;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::{UnixListener, UnixStream};
use tokio::sync::RwLock;
use tracing::{info, warn};

const MAX_IN_MEMORY_HISTORY_ENTRIES: usize = DEFAULT_MAX_IN_MEMORY_HISTORY_ENTRIES;
const MAX_INDEXED_ROOTS: usize = DEFAULT_MAX_INDEXED_ROOTS;
const MAX_INDEXED_ENTRIES_PER_ROOT: usize = DEFAULT_MAX_INDEXED_ENTRIES_PER_ROOT;

#[derive(Debug)]
pub struct DaemonState {
    socket_path: PathBuf,
    store: RwLock<CanonicalStore>,
    history_entries: RwLock<Vec<HistoryEntry>>,
    context_engine: RwLock<ContextEngine>,
    file_index: RwLock<FileIndex>,
    history_persistence: Option<HistoryStore>,
    ssh_hosts: RwLock<Vec<SshHostProfile>>,
    persistence: Option<SqliteStateStore>,
    secrets: Option<SecretsStore>,
    tmux: TmuxRuntime,
    claw: Option<ClawProvider>,
}

#[derive(Debug, Clone)]
struct AgentExecutionRequest {
    task_id: uuid::Uuid,
    workspace_id: uuid::Uuid,
    session_id: Option<uuid::Uuid>,
    cwd: Option<PathBuf>,
    prompt: String,
    model: Option<String>,
    context_query: Option<String>,
    context_limit: usize,
    permission_mode: AgentPermissionMode,
    allowed_tools: Vec<String>,
}

impl DaemonState {
    pub fn new(socket_path: PathBuf) -> Self {
        Self::new_with_tmux_and_secrets(socket_path, TmuxRuntime::system_default(), None)
    }

    pub fn new_with_tmux(socket_path: PathBuf, tmux: TmuxRuntime) -> Self {
        Self::new_with_tmux_and_secrets(socket_path, tmux, None)
    }

    pub fn new_with_secrets(socket_path: PathBuf, secrets: Option<SecretsStore>) -> Self {
        Self::new_with_tmux_and_secrets(socket_path, TmuxRuntime::system_default(), secrets)
    }

    pub fn new_with_tmux_and_secrets(
        socket_path: PathBuf,
        tmux: TmuxRuntime,
        secrets: Option<SecretsStore>,
    ) -> Self {
        Self::try_new_with_tmux_persistence_secrets_and_claw(
            socket_path,
            tmux,
            None,
            secrets,
            default_claw_provider().expect("default claw discovery should not fail"),
        )
        .expect("in-memory daemon state should always initialize")
    }

    pub fn try_new_with_persistence(
        socket_path: PathBuf,
        persistence: SqliteStateStore,
    ) -> Result<Self, StateError> {
        let secrets = persistence.secrets();
        Self::try_new_with_persistence_and_secrets(socket_path, persistence, secrets)
    }

    pub fn try_new_with_persistence_and_secrets(
        socket_path: PathBuf,
        persistence: SqliteStateStore,
        secrets: Option<SecretsStore>,
    ) -> Result<Self, StateError> {
        Self::try_new_with_tmux_persistence_and_secrets(
            socket_path,
            TmuxRuntime::system_default(),
            Some(persistence),
            secrets,
        )
    }

    pub fn try_new_with_tmux_and_persistence(
        socket_path: PathBuf,
        tmux: TmuxRuntime,
        persistence: Option<SqliteStateStore>,
    ) -> Result<Self, StateError> {
        let secrets = persistence.as_ref().and_then(SqliteStateStore::secrets);
        Self::try_new_with_tmux_persistence_and_secrets(socket_path, tmux, persistence, secrets)
    }

    pub fn try_new_with_tmux_persistence_secrets_and_claw(
        socket_path: PathBuf,
        tmux: TmuxRuntime,
        persistence: Option<SqliteStateStore>,
        secrets: Option<SecretsStore>,
        claw: Option<ClawProvider>,
    ) -> Result<Self, StateError> {
        Self::try_new_with_components(socket_path, tmux, persistence, secrets, claw)
    }

    pub fn try_new_with_tmux_persistence_and_secrets(
        socket_path: PathBuf,
        tmux: TmuxRuntime,
        persistence: Option<SqliteStateStore>,
        secrets: Option<SecretsStore>,
    ) -> Result<Self, StateError> {
        Self::try_new_with_components(
            socket_path,
            tmux,
            persistence,
            secrets,
            default_claw_provider().map_err(|error| StateError::Storage(error.to_string()))?,
        )
    }

    fn try_new_with_components(
        socket_path: PathBuf,
        tmux: TmuxRuntime,
        persistence: Option<SqliteStateStore>,
        secrets: Option<SecretsStore>,
        claw: Option<ClawProvider>,
    ) -> Result<Self, StateError> {
        let store = match persistence.as_ref() {
            Some(persistence) => persistence
                .load_store()?
                .unwrap_or_else(CanonicalStore::new_in_memory),
            None => CanonicalStore::new_in_memory(),
        };
        let history_secrets = secrets
            .clone()
            .or_else(|| persistence.as_ref().and_then(SqliteStateStore::secrets));
        let history_persistence = history_db_path(&persistence)?
            .map(|path| HistoryStore::open_with_secrets(path, history_secrets.clone()))
            .transpose()
            .map_err(history_store_to_state_error)?;
        let history_entries = match history_persistence.as_ref() {
            Some(history) => history
                .recent(None, None, MAX_IN_MEMORY_HISTORY_ENTRIES)
                .map_err(history_store_to_state_error)?,
            None => Vec::new(),
        };
        let context_engine = ContextEngine::from_history(&history_entries);

        Ok(Self {
            socket_path,
            store: RwLock::new(store),
            history_entries: RwLock::new(history_entries),
            context_engine: RwLock::new(context_engine),
            file_index: RwLock::new(FileIndex::new()),
            history_persistence,
            ssh_hosts: RwLock::new(Vec::new()),
            persistence,
            secrets,
            tmux,
            claw,
        })
    }

    pub async fn health_snapshot(&self) -> HealthSnapshot {
        let snapshot = self.store.read().await.health_snapshot();

        HealthSnapshot {
            status: RuntimeStatus::Ready,
            daemon: DaemonInfo {
                socket_path: self.socket_path.display().to_string(),
                api_version: runtime_api::API_VERSION.to_string(),
                schema_version: snapshot.schema_version,
            },
            workspace_count: snapshot.workspace_count,
            session_count: snapshot.session_count,
        }
    }

    async fn ping(&self) {
        self.store.write().await.ping();
    }

    async fn register_workspace(
        &self,
        workspace_id: uuid::Uuid,
        name: String,
        roots: Vec<PathBuf>,
    ) -> Result<usize, StateError> {
        let roots = canonicalize_workspace_roots(roots)?;
        self.commit_store_change(move |store| {
            store.register_workspace(workspace_id, name.clone(), roots.clone())?;
            Ok(store.workspace_count())
        })
        .await
    }

    async fn import_ssh_config(&self, config_text: String) -> Result<usize, SshError> {
        let mut imported_hosts: Vec<_> = parse_ssh_config(&config_text)?
            .into_iter()
            .map(map_ssh_host_profile)
            .collect();
        imported_hosts.sort_by(|left, right| left.alias.cmp(&right.alias));

        let host_count = imported_hosts.len();
        let mut ssh_hosts = self.ssh_hosts.write().await;
        *ssh_hosts = imported_hosts;
        Ok(host_count)
    }

    async fn append_history_entry(&self, entry: HistoryEntry) -> Result<usize, HistoryStoreError> {
        let context_entry = entry.clone();
        if let Some(history) = self.history_persistence.clone() {
            let entry_to_persist = entry.clone();
            tokio::task::spawn_blocking(move || history.append(&entry_to_persist))
                .await
                .map_err(|error| HistoryStoreError::Storage(error.to_string()))??;
        }

        let mut entries = self.history_entries.write().await;
        entries.push(entry);
        entries.sort_by(|left, right| {
            right
                .at_unix_ms
                .cmp(&left.at_unix_ms)
                .then(right.id.cmp(&left.id))
        });
        let maybe_rebuild = if entries.len() > MAX_IN_MEMORY_HISTORY_ENTRIES {
            entries.truncate(MAX_IN_MEMORY_HISTORY_ENTRIES);
            Some(entries.clone())
        } else {
            None
        };

        let mut context_engine = self.context_engine.write().await;
        if let Some(retained_entries) = maybe_rebuild {
            *context_engine = ContextEngine::from_history(&retained_entries);
        } else {
            context_engine.append(context_entry);
        }

        Ok(entries.len())
    }

    async fn register_session(
        &self,
        request: RegisterSessionRequest,
    ) -> Result<(Session, usize), SessionBrokerError> {
        self.commit_store_change(move |store| {
            let registration = register_session(store, request.clone())?;
            Ok((registration.session, store.session_count()))
        })
        .await
    }

    async fn register_local_tmux_session(
        &self,
        request: RegisterSessionRequest,
    ) -> Result<(Session, usize), SessionBrokerError> {
        loop {
            let (expected_revision, planned) = {
                let store = self.store.read().await;
                let planned = plan_local_tmux_session(request.clone())?;
                preflight_session_registration(&store, &planned.session)?;
                (store.revision(), planned)
            };

            self.run_tmux_create(
                planned.tmux_session_name.clone(),
                planned.session.cwd.clone(),
            )
            .await?;

            let mut candidate = {
                let store = self.store.read().await;
                if store.revision() != expected_revision {
                    self.cleanup_tmux_session(planned.tmux_session_name.clone())
                        .await;
                    continue;
                }
                store.clone()
            };

            let registration =
                commit_session_registration(&mut candidate, planned.session.clone())?;
            if let Err(error) = self.persist_candidate_store(&candidate).await {
                self.cleanup_tmux_session(planned.tmux_session_name).await;
                return Err(SessionBrokerError::from(error));
            }

            let mut store = self.store.write().await;
            if store.revision() != expected_revision {
                drop(store);
                self.cleanup_tmux_session(planned.tmux_session_name).await;
                continue;
            }

            *store = candidate;
            return Ok((registration.session, store.session_count()));
        }
    }

    async fn remove_session(&self, session_id: uuid::Uuid) -> Result<usize, SessionBrokerError> {
        let tmux_cleanup = {
            let store = self.store.read().await;
            store
                .session(session_id)
                .filter(|session| {
                    session.kind == core_domain::SessionKind::Tmux
                        && session.backing == core_domain::SessionBacking::TmuxSession
                })
                .map(|_| tmux_bridge::TmuxSessionName::new(format!("edex-{}", session_id.simple())))
                .transpose()?
        };

        if let Some(session_name) = tmux_cleanup {
            self.cleanup_tmux_session(session_name).await;
        }

        self.commit_store_change(move |store| {
            remove_session(store, session_id)?;
            Ok(store.session_count())
        })
        .await
    }

    async fn list_sessions(&self, workspace_id: Option<uuid::Uuid>) -> Vec<Session> {
        let store = self.store.read().await;
        list_sessions(&store, SessionFilter { workspace_id })
    }

    async fn ssh_hosts(&self) -> Vec<SshHostProfile> {
        self.ssh_hosts.read().await.clone()
    }

    async fn workspace_roots_for(
        &self,
        workspace_id: uuid::Uuid,
    ) -> Result<Vec<PathBuf>, ApiError> {
        let store = self.store.read().await;
        let workspace = store.workspace(workspace_id).ok_or_else(|| {
            ApiError::new(
                ErrorCode::NotFound,
                format!("workspace `{workspace_id}` is not registered"),
            )
        })?;
        if workspace.roots.is_empty() {
            return Err(policy_error_to_api(PolicyError::NoWorkspaceRoots));
        }

        Ok(workspace.roots.clone())
    }

    async fn workspaces(&self) -> Vec<WorkspaceSummary> {
        self.store
            .read()
            .await
            .workspaces()
            .into_iter()
            .map(|workspace| WorkspaceSummary {
                id: workspace.id,
                name: workspace.name,
                roots: workspace
                    .roots
                    .into_iter()
                    .map(|path| path.display().to_string())
                    .collect(),
                bookmarks: workspace
                    .bookmarks
                    .into_iter()
                    .map(|path| path.display().to_string())
                    .collect(),
            })
            .collect()
    }

    async fn agent_provider_status(&self) -> runtime_api::AgentProviderStatus {
        let Some(provider) = self.claw.clone() else {
            return runtime_api::AgentProviderStatus {
                provider: "claw".into(),
                available: false,
                binary_path: None,
                version_report: None,
                doctor_report: None,
                status_report: None,
            };
        };

        let binary_path = provider.binary_path().display().to_string();
        match tokio::task::spawn_blocking(move || provider.probe()).await {
            Ok(Ok(report)) => map_claw_status_report(report),
            Ok(Err(error)) => runtime_api::AgentProviderStatus {
                provider: "claw".into(),
                available: false,
                binary_path: Some(binary_path),
                version_report: None,
                doctor_report: Some(error.to_string()),
                status_report: None,
            },
            Err(error) => runtime_api::AgentProviderStatus {
                provider: "claw".into(),
                available: false,
                binary_path: Some(binary_path),
                version_report: None,
                doctor_report: Some(error.to_string()),
                status_report: None,
            },
        }
    }

    async fn run_agent_task(
        &self,
        request: AgentExecutionRequest,
    ) -> Result<(AgentTask, String, usize, usize), ApiError> {
        let AgentPolicyDecision {
            permission_mode,
            allowed_tools,
        } = enforce_agent_policy(request.permission_mode, &request.allowed_tools)
            .map_err(policy_error_to_api)?;
        let task = self
            .build_agent_task(&AgentExecutionRequest {
                permission_mode,
                allowed_tools,
                ..request
            })
            .await?;
        let context_results = self
            .context_search(
                Some(task.workspace_id),
                task.session_id,
                task.context_query
                    .clone()
                    .unwrap_or_else(|| task.prompt.clone()),
                task.context_limit,
            )
            .await;
        let rendered_prompt = build_agent_prompt(&task, &context_results);
        let Some(provider) = self.claw.clone() else {
            return Err(ApiError::new(
                ErrorCode::NotFound,
                "claw provider is not configured",
            ));
        };

        let task_for_provider = task.clone();
        let output = tokio::task::spawn_blocking(move || {
            provider.run_task(&task_for_provider, &rendered_prompt)
        })
        .await
        .map_err(|error| ApiError::new(ErrorCode::Internal, error.to_string()))?
        .map_err(claw_error_to_api)?
        .output;

        let at_unix_ms = unix_ms_now();
        self.append_history_entry(
            HistoryEntry::new(
                uuid::Uuid::new_v4(),
                task.workspace_id,
                task.session_id,
                HistoryEntryKind::ChatUser,
                at_unix_ms,
                task.prompt.clone(),
            )
            .map_err(|error| ApiError::new(ErrorCode::ValidationFailed, error.to_string()))?,
        )
        .await
        .map_err(history_error_to_api)?;
        let history_count = self
            .append_history_entry(
                HistoryEntry::new(
                    uuid::Uuid::new_v4(),
                    task.workspace_id,
                    task.session_id,
                    HistoryEntryKind::ChatAgent,
                    at_unix_ms.saturating_add(1),
                    output.clone(),
                )
                .map_err(|error| ApiError::new(ErrorCode::ValidationFailed, error.to_string()))?,
            )
            .await
            .map_err(history_error_to_api)?;

        Ok((task, output, context_results.len(), history_count))
    }

    async fn build_agent_task(
        &self,
        request: &AgentExecutionRequest,
    ) -> Result<AgentTask, ApiError> {
        let cwd = {
            let store = self.store.read().await;
            let workspace = store.workspace(request.workspace_id).ok_or_else(|| {
                ApiError::new(
                    ErrorCode::NotFound,
                    format!("workspace `{}` is not registered", request.workspace_id),
                )
            })?;
            if workspace.roots.is_empty() {
                return Err(policy_error_to_api(PolicyError::NoWorkspaceRoots));
            }

            let session_cwd = match request.session_id {
                Some(session_id) => {
                    let session = store.session(session_id).ok_or_else(|| {
                        ApiError::new(
                            ErrorCode::NotFound,
                            format!("session `{session_id}` is not registered"),
                        )
                    })?;
                    if session.workspace_id != request.workspace_id {
                        return Err(ApiError::new(
                            ErrorCode::ValidationFailed,
                            format!(
                                "session `{session_id}` does not belong to workspace `{}`",
                                request.workspace_id
                            ),
                        ));
                    }
                    Some(session.cwd.clone())
                }
                None => None,
            };

            let cwd = request
                .cwd
                .clone()
                .or(session_cwd)
                .or_else(|| workspace.roots.first().cloned())
                .ok_or_else(|| policy_error_to_api(PolicyError::NoWorkspaceRoots))?;
            let cwd = canonicalize_host_path(&cwd).map_err(file_index_error_to_api)?;
            ensure_path_within_roots(&cwd, &workspace.roots).map_err(policy_error_to_api)?;
            cwd
        };

        AgentTask::new(AgentTaskDraft {
            id: request.task_id,
            workspace_id: request.workspace_id,
            session_id: request.session_id,
            cwd,
            prompt: request.prompt.clone(),
            model: request.model.clone(),
            context_query: request.context_query.clone(),
            context_limit: request.context_limit,
            permission_mode: request.permission_mode,
            allowed_tools: request.allowed_tools.clone(),
        })
        .map_err(|error| ApiError::new(ErrorCode::ValidationFailed, error.to_string()))
    }

    async fn recent_history(
        &self,
        workspace_id: Option<uuid::Uuid>,
        session_id: Option<uuid::Uuid>,
        limit: usize,
    ) -> Vec<HistoryEntry> {
        self.history_entries
            .read()
            .await
            .iter()
            .filter(|entry| {
                workspace_id
                    .map(|id| entry.workspace_id == id)
                    .unwrap_or(true)
            })
            .filter(|entry| {
                session_id
                    .map(|id| entry.session_id == Some(id))
                    .unwrap_or(true)
            })
            .take(limit)
            .cloned()
            .collect()
    }

    async fn context_search(
        &self,
        workspace_id: Option<uuid::Uuid>,
        session_id: Option<uuid::Uuid>,
        text: String,
        limit: usize,
    ) -> Vec<ContextResult> {
        let engine = self.context_engine.read().await;
        engine.search(&ContextQuery {
            workspace_id,
            session_id,
            text,
            limit,
        })
    }

    async fn refresh_file_index(
        &self,
        workspace_id: uuid::Uuid,
        root_path: PathBuf,
    ) -> Result<usize, ApiError> {
        let root_path = canonicalize_host_path(&root_path).map_err(file_index_error_to_api)?;
        let workspace_roots = self.workspace_roots_for(workspace_id).await?;
        ensure_path_within_roots(&root_path, &workspace_roots)
            .map_err(policy_error_to_api)?;

        let mut file_index = self.file_index.write().await;
        if !file_index
            .contains_root(&root_path)
            .map_err(file_index_error_to_api)?
            && file_index.root_count() >= MAX_INDEXED_ROOTS
        {
            return Err(ApiError::new(
                ErrorCode::Busy,
                format!("indexed root quota reached: {}", MAX_INDEXED_ROOTS),
            ));
        }
        let mut next = file_index.clone();
        let indexed = tokio::task::spawn_blocking(move || {
            let indexed_count = next.refresh_root_with_limit(&root_path, MAX_INDEXED_ENTRIES_PER_ROOT)?;
            Ok::<_, FileIndexError>((next, indexed_count))
        })
        .await
        .map_err(|error| ApiError::new(ErrorCode::Internal, error.to_string()))?
        .map_err(file_index_error_to_api)?;

        *file_index = indexed.0;
        Ok(indexed.1)
    }

    async fn file_list(
        &self,
        workspace_id: uuid::Uuid,
        path: PathBuf,
        limit: usize,
    ) -> Result<Vec<FileEntry>, ApiError> {
        let path = canonicalize_host_path(&path).map_err(file_index_error_to_api)?;
        let workspace_roots = self.workspace_roots_for(workspace_id).await?;
        ensure_path_within_roots(&path, &workspace_roots).map_err(policy_error_to_api)?;
        let file_index = self.file_index.read().await.clone();
        tokio::task::spawn_blocking(move || file_index.list_dir(&path, limit))
            .await
            .map_err(|error| ApiError::new(ErrorCode::Internal, error.to_string()))?
            .map_err(file_index_error_to_api)
    }

    async fn file_stat(
        &self,
        workspace_id: uuid::Uuid,
        path: PathBuf,
    ) -> Result<FileEntry, ApiError> {
        let path = canonicalize_host_path(&path).map_err(file_index_error_to_api)?;
        let workspace_roots = self.workspace_roots_for(workspace_id).await?;
        ensure_path_within_roots(&path, &workspace_roots).map_err(policy_error_to_api)?;
        let file_index = self.file_index.read().await.clone();
        tokio::task::spawn_blocking(move || file_index.stat_path(&path))
            .await
            .map_err(|error| ApiError::new(ErrorCode::Internal, error.to_string()))?
            .map_err(file_index_error_to_api)
    }

    async fn file_preview(
        &self,
        workspace_id: uuid::Uuid,
        path: PathBuf,
        max_bytes: usize,
        max_lines: usize,
    ) -> Result<FilePreview, ApiError> {
        let path = canonicalize_host_path(&path).map_err(file_index_error_to_api)?;
        let workspace_roots = self.workspace_roots_for(workspace_id).await?;
        ensure_path_within_roots(&path, &workspace_roots).map_err(policy_error_to_api)?;
        let file_index = self.file_index.read().await.clone();
        let bounded_bytes = max_bytes.clamp(1, DEFAULT_MAX_FILE_PREVIEW_BYTES);
        let bounded_lines = max_lines.clamp(1, DEFAULT_MAX_FILE_PREVIEW_LINES);
        tokio::task::spawn_blocking(move || file_index.preview_file(&path, bounded_bytes, bounded_lines))
            .await
            .map_err(|error| ApiError::new(ErrorCode::Internal, error.to_string()))?
            .map_err(file_index_error_to_api)
    }

    async fn file_search(
        &self,
        workspace_id: uuid::Uuid,
        root_path: PathBuf,
        text: String,
        limit: usize,
    ) -> Result<Vec<FileSearchResult>, ApiError> {
        let root_path = canonicalize_host_path(&root_path).map_err(file_index_error_to_api)?;
        let workspace_roots = self.workspace_roots_for(workspace_id).await?;
        ensure_path_within_roots(&root_path, &workspace_roots).map_err(policy_error_to_api)?;
        let file_index = self.file_index.read().await.clone();
        tokio::task::spawn_blocking(move || file_index.search(&root_path, &text, limit))
            .await
            .map_err(|error| ApiError::new(ErrorCode::Internal, error.to_string()))?
            .map_err(file_index_error_to_api)
    }

    async fn export_recovery_bundle(
        &self,
        bundle_path: PathBuf,
    ) -> Result<RecoveryBundleSummary, RecoveryError> {
        let bundle = RecoveryBundle::new(
            self.store.read().await.snapshot(),
            self.history_entries.read().await.clone(),
            self.ssh_hosts.read().await.clone(),
        );
        let secrets = self.recovery_secrets()?;

        tokio::task::spawn_blocking(move || export_bundle(&bundle_path, &bundle, &secrets))
            .await
            .map_err(|error| RecoveryError::Storage(error.to_string()))?
    }

    async fn import_recovery_bundle(
        &self,
        bundle_path: PathBuf,
    ) -> Result<RecoveryBundleSummary, RecoveryError> {
        let secrets = self.recovery_secrets()?;
        let bundle_path_for_import = bundle_path.clone();
        let bundle =
            tokio::task::spawn_blocking(move || import_bundle(&bundle_path_for_import, &secrets))
                .await
                .map_err(|error| RecoveryError::Storage(error.to_string()))??;
        let summary = bundle.summary_for_path(bundle_path);

        if let Some(persistence) = self.persistence.clone() {
            let snapshot = bundle.state_snapshot.clone();
            tokio::task::spawn_blocking(move || persistence.save_snapshot(&snapshot))
                .await
                .map_err(|error| RecoveryError::Storage(error.to_string()))?
                .map_err(recovery_from_state_error)?;
        }

        if let Some(history) = self.history_persistence.clone() {
            let entries = bundle.history_entries.clone();
            tokio::task::spawn_blocking(move || history.replace_all(&entries))
                .await
                .map_err(|error| RecoveryError::Storage(error.to_string()))?
                .map_err(recovery_from_history_error)?;
        }

        let store = CanonicalStore::from_snapshot(bundle.state_snapshot);
        let context = ContextEngine::from_history(&bundle.history_entries);

        *self.store.write().await = store;
        *self.history_entries.write().await = bundle.history_entries;
        *self.context_engine.write().await = context;
        *self.ssh_hosts.write().await = bundle.ssh_hosts;

        Ok(summary)
    }

    async fn rekey_recovery_bundle(
        &self,
        input_path: PathBuf,
        output_path: PathBuf,
        new_passphrase: String,
    ) -> Result<RecoveryBundleSummary, RecoveryError> {
        let current_secrets = self.recovery_secrets()?;
        let next_secrets = SecretsStore::from_owned_passphrase(new_passphrase)?;

        tokio::task::spawn_blocking(move || {
            rekey_bundle(&input_path, &output_path, &current_secrets, &next_secrets)
        })
        .await
        .map_err(|error| RecoveryError::Storage(error.to_string()))?
    }

    fn recovery_secrets(&self) -> Result<SecretsStore, RecoveryError> {
        self.secrets.clone().ok_or(RecoveryError::MissingSecrets)
    }

    async fn commit_store_change<T, E, F>(&self, mutator: F) -> Result<T, E>
    where
        F: Fn(&mut CanonicalStore) -> Result<T, E>,
        E: From<StateError>,
    {
        loop {
            let (expected_revision, mut candidate) = {
                let store = self.store.read().await;
                (store.revision(), store.clone())
            };

            let result = mutator(&mut candidate)?;
            self.persist_candidate_store(&candidate)
                .await
                .map_err(E::from)?;

            let mut store = self.store.write().await;
            if store.revision() != expected_revision {
                continue;
            }

            *store = candidate;
            return Ok(result);
        }
    }

    async fn persist_candidate_store(&self, candidate: &CanonicalStore) -> Result<(), StateError> {
        let Some(persistence) = self.persistence.clone() else {
            return Ok(());
        };

        let snapshot = candidate.snapshot();
        tokio::task::spawn_blocking(move || persistence.save_snapshot(&snapshot))
            .await
            .map_err(|error| StateError::Storage(error.to_string()))??;

        Ok(())
    }

    async fn run_tmux_create(
        &self,
        session_name: tmux_bridge::TmuxSessionName,
        cwd: PathBuf,
    ) -> Result<(), SessionBrokerError> {
        let tmux = self.tmux.clone();
        tokio::task::spawn_blocking(move || tmux.create_session(&session_name, &cwd))
            .await
            .map_err(|error| SessionBrokerError::BlockingTask(error.to_string()))??;
        Ok(())
    }

    async fn cleanup_tmux_session(&self, session_name: tmux_bridge::TmuxSessionName) {
        let tmux = self.tmux.clone();
        let session_label = session_name.as_str().to_owned();
        match tokio::task::spawn_blocking(move || tmux.kill_session(&session_name)).await {
            Ok(Ok(())) => {}
            Ok(Err(error)) => {
                warn!(error = %error, session_name = %session_label, "failed to clean up orphaned tmux session")
            }
            Err(error) => {
                warn!(error = %error, session_name = %session_label, "tmux cleanup task failed")
            }
        }
    }
}

pub async fn run_daemon(listener: UnixListener, state: Arc<DaemonState>) -> Result<()> {
    info!(socket_path = %state.socket_path.display(), "runtime-daemon listening");

    loop {
        let (stream, _) = listener.accept().await.context("accept uds connection")?;
        let state = Arc::clone(&state);

        tokio::spawn(async move {
            if let Err(error) = handle_connection(stream, state).await {
                warn!(error = %error, "connection handler failed");
            }
        });
    }
}

async fn handle_connection(stream: UnixStream, state: Arc<DaemonState>) -> Result<()> {
    let (reader, mut writer) = stream.into_split();
    let mut lines = BufReader::new(reader).lines();

    while let Some(line) = lines.next_line().await.context("read request line")? {
        if line.trim().is_empty() {
            continue;
        }

        let request = match decode_json_frame::<RequestEnvelope>(&line) {
            Ok(Some(request)) => request,
            Ok(None) => continue,
            Err(error) => {
                let response = ResponseEnvelope::error("invalid", frame_error_to_api(error));
                let encoded =
                    encode_json_frame(&response).context("encode invalid request response")?;
                writer
                    .write_all(&encoded)
                    .await
                    .context("write invalid request response")?;
                writer
                    .flush()
                    .await
                    .context("flush invalid request response")?;
                continue;
            }
        };
        let response = handle_request(&state, request).await;
        let encoded = encode_json_frame(&response).context("encode response envelope")?;

        writer
            .write_all(&encoded)
            .await
            .context("write response payload")?;
        writer.flush().await.context("flush response")?;
    }

    Ok(())
}

async fn handle_request(state: &DaemonState, request: RequestEnvelope) -> ResponseEnvelope {
    match request.payload {
        runtime_api::RequestPayload::Command(command) => {
            handle_command(state, request.request_id, command).await
        }
        runtime_api::RequestPayload::Query(query) => {
            handle_query(state, request.request_id, query).await
        }
    }
}

async fn handle_command(
    state: &DaemonState,
    request_id: String,
    command: Command,
) -> ResponseEnvelope {
    match command {
        Command::Ping => {
            state.ping().await;
            ResponseEnvelope::ok(request_id, ResponsePayload::Pong)
        }
        Command::RunAgentTask {
            task_id,
            workspace_id,
            session_id,
            cwd,
            prompt,
            model,
            context_query,
            context_limit,
            permission_mode,
            allowed_tools,
        } => match state
            .run_agent_task(AgentExecutionRequest {
                task_id,
                workspace_id,
                session_id,
                cwd: cwd.map(PathBuf::from),
                prompt,
                model,
                context_query,
                context_limit,
                permission_mode,
                allowed_tools,
            })
            .await
        {
            Ok((task, output, context_result_count, history_count)) => ResponseEnvelope::ok(
                request_id,
                ResponsePayload::AgentTaskCompleted {
                    task,
                    output,
                    context_result_count,
                    history_count,
                },
            ),
            Err(error) => ResponseEnvelope::error(request_id, error),
        },
        Command::AppendHistoryEntry {
            entry_id,
            workspace_id,
            session_id,
            kind,
            at_unix_ms,
            content,
        } => match HistoryEntry::new(
            entry_id,
            workspace_id,
            session_id,
            kind,
            at_unix_ms,
            content,
        ) {
            Ok(entry) => match state.append_history_entry(entry).await {
                Ok(history_count) => ResponseEnvelope::ok(
                    request_id,
                    ResponsePayload::HistoryAppended { history_count },
                ),
                Err(error) => ResponseEnvelope::error(request_id, history_error_to_api(error)),
            },
            Err(error) => ResponseEnvelope::error(
                request_id,
                ApiError::new(ErrorCode::ValidationFailed, error.to_string()),
            ),
        },
        Command::RegisterWorkspace {
            workspace_id,
            name,
            roots,
        } => {
            match state
                .register_workspace(
                    workspace_id,
                    name,
                    roots.into_iter().map(PathBuf::from).collect(),
                )
                .await
            {
                Ok(total) => ResponseEnvelope::ok(
                    request_id,
                    ResponsePayload::WorkspaceRegistered {
                        workspace_count: total,
                    },
                ),
                Err(error) => ResponseEnvelope::error(request_id, state_error_to_api(error)),
            }
        }
        Command::ImportSshConfig { config_text } => {
            match state.import_ssh_config(config_text).await {
                Ok(host_count) => ResponseEnvelope::ok(
                    request_id,
                    ResponsePayload::SshConfigImported { host_count },
                ),
                Err(error) => ResponseEnvelope::error(request_id, ssh_error_to_api(error)),
            }
        }
        Command::RefreshFileIndex {
            workspace_id,
            root_path,
        } => {
            match state
                .refresh_file_index(workspace_id, PathBuf::from(&root_path))
                .await
            {
                Ok(indexed_count) => ResponseEnvelope::ok(
                    request_id,
                    ResponsePayload::FileIndexRefreshed {
                        root_path,
                        indexed_count,
                    },
                ),
                Err(error) => ResponseEnvelope::error(request_id, error),
            }
        }
        Command::ExportRecoveryBundle { bundle_path } => {
            match state
                .export_recovery_bundle(PathBuf::from(bundle_path))
                .await
            {
                Ok(summary) => ResponseEnvelope::ok(
                    request_id,
                    ResponsePayload::RecoveryBundleExported {
                        bundle_path: summary.bundle_path.display().to_string(),
                        workspace_count: summary.workspace_count,
                        session_count: summary.session_count,
                        history_count: summary.history_count,
                        ssh_host_count: summary.ssh_host_count,
                    },
                ),
                Err(error) => ResponseEnvelope::error(request_id, recovery_error_to_api(error)),
            }
        }
        Command::ImportRecoveryBundle { bundle_path } => {
            match state
                .import_recovery_bundle(PathBuf::from(bundle_path))
                .await
            {
                Ok(summary) => ResponseEnvelope::ok(
                    request_id,
                    ResponsePayload::RecoveryBundleImported {
                        bundle_path: summary.bundle_path.display().to_string(),
                        workspace_count: summary.workspace_count,
                        session_count: summary.session_count,
                        history_count: summary.history_count,
                        ssh_host_count: summary.ssh_host_count,
                    },
                ),
                Err(error) => ResponseEnvelope::error(request_id, recovery_error_to_api(error)),
            }
        }
        Command::RekeyRecoveryBundle {
            input_path,
            output_path,
            new_passphrase,
        } => match state
            .rekey_recovery_bundle(
                PathBuf::from(input_path),
                PathBuf::from(output_path),
                new_passphrase,
            )
            .await
        {
            Ok(summary) => ResponseEnvelope::ok(
                request_id,
                ResponsePayload::RecoveryBundleRekeyed {
                    output_path: summary.bundle_path.display().to_string(),
                    workspace_count: summary.workspace_count,
                    session_count: summary.session_count,
                    history_count: summary.history_count,
                    ssh_host_count: summary.ssh_host_count,
                },
            ),
            Err(error) => ResponseEnvelope::error(request_id, recovery_error_to_api(error)),
        },
        Command::RegisterLocalTmuxSession {
            session_id,
            workspace_id,
            cwd,
        } => match state
            .register_local_tmux_session(RegisterSessionRequest {
                session_id,
                workspace_id,
                kind: core_domain::SessionKind::Tmux,
                backing: core_domain::SessionBacking::TmuxSession,
                cwd: PathBuf::from(cwd),
            })
            .await
        {
            Ok((session, total)) => ResponseEnvelope::ok(
                request_id,
                ResponsePayload::SessionRegistered {
                    session,
                    session_count: total,
                },
            ),
            Err(error) => ResponseEnvelope::error(request_id, session_error_to_api(error)),
        },
        Command::RegisterSession {
            session_id,
            workspace_id,
            kind,
            backing,
            cwd,
        } => match state
            .register_session(RegisterSessionRequest {
                session_id,
                workspace_id,
                kind,
                backing,
                cwd: PathBuf::from(cwd),
            })
            .await
        {
            Ok((session, total)) => ResponseEnvelope::ok(
                request_id,
                ResponsePayload::SessionRegistered {
                    session,
                    session_count: total,
                },
            ),
            Err(error) => ResponseEnvelope::error(request_id, session_error_to_api(error)),
        },
        Command::RemoveSession { session_id } => match state.remove_session(session_id).await {
            Ok(total) => ResponseEnvelope::ok(
                request_id,
                ResponsePayload::SessionRemoved {
                    session_id,
                    session_count: total,
                },
            ),
            Err(error) => ResponseEnvelope::error(request_id, session_error_to_api(error)),
        },
    }
}

async fn handle_query(state: &DaemonState, request_id: String, query: Query) -> ResponseEnvelope {
    match query {
        Query::Health => {
            let health = state.health_snapshot().await;
            ResponseEnvelope::ok(request_id, ResponsePayload::Health(health))
        }
        Query::AgentProviderStatus => {
            let status = state.agent_provider_status().await;
            ResponseEnvelope::ok(request_id, ResponsePayload::AgentProviderStatus { status })
        }
        Query::Workspaces => {
            let workspaces = state.workspaces().await;
            ResponseEnvelope::ok(request_id, ResponsePayload::Workspaces { workspaces })
        }
        Query::RecentHistory {
            workspace_id,
            session_id,
            limit,
        } => {
            let entries = state.recent_history(workspace_id, session_id, limit).await;
            ResponseEnvelope::ok(request_id, ResponsePayload::HistoryEntries { entries })
        }
        Query::FileList {
            workspace_id,
            path,
            limit,
        } => {
            match state.file_list(workspace_id, PathBuf::from(path), limit).await {
                Ok(entries) => {
                    ResponseEnvelope::ok(request_id, ResponsePayload::FileEntries { entries })
                }
                Err(error) => ResponseEnvelope::error(request_id, error),
            }
        }
        Query::FileStat { workspace_id, path } => {
            match state.file_stat(workspace_id, PathBuf::from(path)).await {
            Ok(entry) => ResponseEnvelope::ok(request_id, ResponsePayload::FileMetadata { entry }),
            Err(error) => ResponseEnvelope::error(request_id, error),
        }
        }
        Query::FilePreview {
            workspace_id,
            path,
            max_bytes,
            max_lines,
        } => match state
            .file_preview(workspace_id, PathBuf::from(path), max_bytes, max_lines)
            .await
        {
            Ok(preview) => {
                ResponseEnvelope::ok(request_id, ResponsePayload::FilePreview { preview })
            }
            Err(error) => ResponseEnvelope::error(request_id, error),
        },
        Query::FileSearch {
            workspace_id,
            root_path,
            text,
            limit,
        } => match state
            .file_search(workspace_id, PathBuf::from(root_path), text, limit)
            .await
        {
            Ok(results) => {
                ResponseEnvelope::ok(request_id, ResponsePayload::FileSearchResults { results })
            }
            Err(error) => ResponseEnvelope::error(request_id, error),
        },
        Query::ContextSearch {
            workspace_id,
            session_id,
            text,
            limit,
        } => {
            let results = state
                .context_search(workspace_id, session_id, text, limit)
                .await;
            ResponseEnvelope::ok(request_id, ResponsePayload::ContextResults { results })
        }
        Query::WorkspaceCount => {
            let health = state.health_snapshot().await;
            ResponseEnvelope::ok(
                request_id,
                ResponsePayload::WorkspaceCount {
                    workspace_count: health.workspace_count,
                },
            )
        }
        Query::SshHosts => {
            let hosts = state.ssh_hosts().await;
            ResponseEnvelope::ok(request_id, ResponsePayload::SshHosts { hosts })
        }
        Query::Sessions { workspace_id } => {
            let sessions = state.list_sessions(workspace_id).await;
            ResponseEnvelope::ok(request_id, ResponsePayload::Sessions { sessions })
        }
    }
}

pub async fn bind_listener(socket_path: &Path) -> Result<UnixListener> {
    if socket_path.exists() {
        std::fs::remove_file(socket_path)
            .with_context(|| format!("remove stale socket at {}", socket_path.display()))?;
    }

    UnixListener::bind(socket_path)
        .with_context(|| format!("bind uds listener at {}", socket_path.display()))
}

fn state_error_to_api(error: StateError) -> ApiError {
    match error {
        StateError::Domain(domain_error) => {
            ApiError::new(ErrorCode::ValidationFailed, domain_error.to_string())
        }
        StateError::InvalidWorkspaceRoot(path) => ApiError::new(
            ErrorCode::ValidationFailed,
            format!("workspace root `{path}` is not accessible"),
        ),
        StateError::Storage(message) => ApiError::new(ErrorCode::Internal, message),
        StateError::WorkspaceAlreadyExists(workspace_id) => ApiError::new(
            ErrorCode::ValidationFailed,
            format!("workspace `{workspace_id}` is already registered"),
        ),
        StateError::WorkspaceNotFound(workspace_id) => ApiError::new(
            ErrorCode::NotFound,
            format!("workspace `{workspace_id}` is not registered"),
        ),
        StateError::SessionAlreadyExists(session_id) => ApiError::new(
            ErrorCode::ValidationFailed,
            format!("session `{session_id}` is already registered"),
        ),
        StateError::SessionNotFound(session_id) => ApiError::new(
            ErrorCode::NotFound,
            format!("session `{session_id}` is not registered"),
        ),
    }
}

fn session_error_to_api(error: SessionBrokerError) -> ApiError {
    match error {
        SessionBrokerError::Domain(domain_error) => {
            ApiError::new(ErrorCode::ValidationFailed, domain_error.to_string())
        }
        SessionBrokerError::State(state_error) => state_error_to_api(state_error),
        SessionBrokerError::Tmux(tmux_error) => {
            ApiError::new(ErrorCode::Internal, tmux_error.to_string())
        }
        SessionBrokerError::BlockingTask(message) => ApiError::new(ErrorCode::Internal, message),
    }
}

fn ssh_error_to_api(error: SshError) -> ApiError {
    match error {
        SshError::EmptyAlias
        | SshError::MalformedConfigLine(_)
        | SshError::UnsupportedGlobalDirective { .. }
        | SshError::UnsupportedDirective { .. }
        | SshError::UnsupportedHostPattern { .. }
        | SshError::InvalidField { .. }
        | SshError::MissingHostName { .. } => {
            ApiError::new(ErrorCode::ValidationFailed, error.to_string())
        }
        SshError::Io(_) | SshError::CommandFailed { .. } => {
            ApiError::new(ErrorCode::Internal, error.to_string())
        }
    }
}

fn history_error_to_api(error: HistoryStoreError) -> ApiError {
    match error {
        HistoryStoreError::Domain(domain_error) => {
            ApiError::new(ErrorCode::ValidationFailed, domain_error.to_string())
        }
        HistoryStoreError::Storage(message) => ApiError::new(ErrorCode::Internal, message),
    }
}

fn recovery_error_to_api(error: RecoveryError) -> ApiError {
    match error {
        RecoveryError::MissingSecrets => {
            ApiError::new(ErrorCode::ValidationFailed, error.to_string())
        }
        RecoveryError::Secrets(secrets_error) => {
            ApiError::new(ErrorCode::ValidationFailed, secrets_error.to_string())
        }
        RecoveryError::Storage(message) => ApiError::new(ErrorCode::Internal, message),
    }
}

fn file_index_error_to_api(error: FileIndexError) -> ApiError {
    match error {
        FileIndexError::NotDirectory(_)
        | FileIndexError::RootNotIndexed(_)
        | FileIndexError::AccessDenied(_) => {
            ApiError::new(ErrorCode::ValidationFailed, error.to_string())
        }
        FileIndexError::EntryLimitExceeded { .. } => {
            ApiError::new(ErrorCode::Busy, error.to_string())
        }
        FileIndexError::PathNotFound(_) => ApiError::new(ErrorCode::NotFound, error.to_string()),
        FileIndexError::Io(message) => ApiError::new(ErrorCode::Internal, message),
    }
}

fn frame_error_to_api(error: FrameError) -> ApiError {
    match error {
        FrameError::FrameTooLarge {
            frame_bytes,
            max_frame_bytes,
        } => ApiError::new(
            ErrorCode::InvalidRequest,
            format!("request frame exceeds max size: {frame_bytes} > {max_frame_bytes}"),
        ),
        FrameError::Encode(_) | FrameError::Decode(_) => {
            ApiError::new(ErrorCode::InvalidRequest, "request frame is not valid json")
        }
    }
}

fn policy_error_to_api(error: PolicyError) -> ApiError {
    match error {
        PolicyError::Denied
        | PolicyError::DangerModeDenied
        | PolicyError::ToolDenied(_)
        | PolicyError::NoWorkspaceRoots
        | PolicyError::PathOutsideWorkspaceRoots { .. } => {
            ApiError::new(ErrorCode::ValidationFailed, error.to_string())
        }
    }
}

fn history_db_path(persistence: &Option<SqliteStateStore>) -> Result<Option<PathBuf>, StateError> {
    let path = match std::env::var("EDEX_CORE_HISTORY_DB") {
        Ok(path) => Some(PathBuf::from(path)),
        Err(_) => persistence.as_ref().map(|state| {
            let mut path = state.db_path().to_path_buf();
            path.set_extension("history.sqlite3");
            path
        }),
    };

    Ok(path)
}

fn history_store_to_state_error(error: HistoryStoreError) -> StateError {
    match error {
        HistoryStoreError::Domain(domain_error) => StateError::Domain(domain_error),
        HistoryStoreError::Storage(message) => StateError::Storage(message),
    }
}

fn canonicalize_host_path(path: &Path) -> Result<PathBuf, FileIndexError> {
    std::fs::canonicalize(path).map_err(|error| FileIndexError::Io(error.to_string()))
}

fn canonicalize_workspace_roots(roots: Vec<PathBuf>) -> Result<Vec<PathBuf>, StateError> {
    roots
        .into_iter()
        .map(|root| {
            std::fs::canonicalize(&root).map_err(|_| {
                StateError::InvalidWorkspaceRoot(root.display().to_string())
            })
        })
        .collect()
}

fn recovery_from_state_error(error: StateError) -> RecoveryError {
    RecoveryError::Storage(error.to_string())
}

fn recovery_from_history_error(error: HistoryStoreError) -> RecoveryError {
    RecoveryError::Storage(error.to_string())
}

fn default_claw_provider() -> Result<Option<ClawProvider>, ClawError> {
    ClawProvider::discover()
}

fn map_ssh_host_profile(host: SshHostSpec) -> SshHostProfile {
    SshHostProfile {
        alias: host.alias,
        hostname: host.hostname,
        user: host.user,
        port: host.port,
        identity_file: host.identity_file.map(|path| path.display().to_string()),
        proxy_jump: host.proxy_jump,
        local_forwards: host
            .local_forwards
            .into_iter()
            .map(map_tcp_forward)
            .collect(),
        remote_forwards: host
            .remote_forwards
            .into_iter()
            .map(map_tcp_forward)
            .collect(),
        dynamic_forwards: host
            .dynamic_forwards
            .into_iter()
            .map(map_dynamic_forward)
            .collect(),
    }
}

fn map_tcp_forward(forward: ssh_bridge::TcpForward) -> SshTcpForward {
    SshTcpForward {
        bind_address: forward.bind_address,
        bind_port: forward.bind_port,
        target_host: forward.target_host,
        target_port: forward.target_port,
    }
}

fn map_dynamic_forward(forward: ssh_bridge::DynamicForward) -> SshDynamicForward {
    SshDynamicForward {
        bind_address: forward.bind_address,
        bind_port: forward.bind_port,
    }
}

fn map_claw_status_report(report: ClawStatusReport) -> runtime_api::AgentProviderStatus {
    runtime_api::AgentProviderStatus {
        provider: "claw".into(),
        available: true,
        binary_path: Some(report.binary_path.display().to_string()),
        version_report: Some(report.version),
        doctor_report: Some(report.doctor),
        status_report: Some(report.status),
    }
}

fn build_agent_prompt(task: &AgentTask, context_results: &[ContextResult]) -> String {
    let mut prompt = String::new();
    prompt.push_str("You are operating inside the eDEX-UI 2026 rust-core agent runtime.\n");
    prompt.push_str(&format!("Workspace ID: {}\n", task.workspace_id));
    if let Some(session_id) = task.session_id {
        prompt.push_str(&format!("Session ID: {session_id}\n"));
    }
    prompt.push_str(&format!("Working directory: {}\n", task.cwd.display()));
    if let Some(query) = task.context_query.as_deref() {
        prompt.push_str(&format!("Context query: {query}\n"));
    }

    if !context_results.is_empty() {
        prompt.push_str("\nRelevant context excerpts:\n");
        for (index, result) in context_results.iter().enumerate() {
            prompt.push_str(&format!(
                "{}. score={:.3} kind={:?} at={} content={}\n",
                index + 1,
                result.score,
                result.kind,
                result.at_unix_ms,
                result.preview
            ));
        }
    }

    prompt.push_str("\nUser task:\n");
    prompt.push_str(&task.prompt);
    prompt
}

fn claw_error_to_api(error: ClawError) -> ApiError {
    match error {
        ClawError::BinaryNotConfigured | ClawError::BinaryMissing(_) => {
            ApiError::new(ErrorCode::NotFound, error.to_string())
        }
        ClawError::CommandFailed { .. } | ClawError::ExitFailure { .. } => {
            ApiError::new(ErrorCode::Internal, error.to_string())
        }
    }
}

fn unix_ms_now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_millis() as u64)
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use claw_bridge::ClawProvider;
    use core_domain::HistoryEntryKind;
    use core_state::SqliteStateStore;
    use runtime_api::{
        Command, Query, RequestEnvelope, RequestPayload, ResponseEnvelope, ResponsePayload,
    };
    use secrecy::SecretString;
    use secrets_store::SecretsStore;
    use std::fs;
    use std::time::{SystemTime, UNIX_EPOCH};
    use tmux_bridge::TmuxRuntime;
    use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
    use tokio::net::UnixStream;
    use uuid::Uuid;

    #[tokio::test]
    async fn ping_and_register_workspace_roundtrip() -> Result<()> {
        let state = Arc::new(DaemonState::new(PathBuf::from("test.sock")));

        let ping = RequestEnvelope {
            request_id: "req-ping".into(),
            payload: RequestPayload::Command(Command::Ping),
        };
        let ping_response = handle_request(&state, ping).await;
        assert!(matches!(ping_response.payload, ResponsePayload::Pong));

        let register = RequestEnvelope {
            request_id: "req-register".into(),
            payload: RequestPayload::Command(Command::RegisterWorkspace {
                workspace_id: Uuid::new_v4(),
                name: "alpha".into(),
                roots: vec![env!("CARGO_MANIFEST_DIR").into()],
            }),
        };
        let register_response = handle_request(&state, register).await;
        assert!(matches!(
            register_response.payload,
            ResponsePayload::WorkspaceRegistered { workspace_count: 1 }
        ));

        let health = RequestEnvelope {
            request_id: "req-health".into(),
            payload: RequestPayload::Query(Query::Health),
        };
        let health_response = handle_request(&state, health).await;
        match health_response.payload {
            ResponsePayload::Health(ref snapshot) => {
                assert_eq!(snapshot.workspace_count, 1);
                assert_eq!(snapshot.session_count, 0);
            }
            other => panic!("unexpected payload: {other:?}"),
        }

        let serialized = serde_json::to_string(&health_response)?;
        let decoded: ResponseEnvelope = serde_json::from_str(&serialized)?;
        assert!(matches!(decoded.payload, ResponsePayload::Health(_)));

        Ok(())
    }

    #[tokio::test]
    async fn session_roundtrip_follows_workspace_registration() -> Result<()> {
        let state = Arc::new(DaemonState::new(PathBuf::from("test.sock")));
        let workspace_id = Uuid::new_v4();
        let session_id = Uuid::new_v4();

        let register_workspace = RequestEnvelope::command(
            "req-workspace",
            Command::RegisterWorkspace {
                workspace_id,
                name: "alpha".into(),
                roots: vec![env!("CARGO_MANIFEST_DIR").into()],
            },
        );
        let workspace_response = handle_request(&state, register_workspace).await;
        assert!(matches!(
            workspace_response.payload,
            ResponsePayload::WorkspaceRegistered { workspace_count: 1 }
        ));

        let start_session = RequestEnvelope::command(
            "req-session-start",
            Command::RegisterSession {
                session_id,
                workspace_id,
                kind: core_domain::SessionKind::Local,
                backing: core_domain::SessionBacking::LocalPty,
                cwd: "/workspace".into(),
            },
        );
        let start_response = handle_request(&state, start_session).await;
        match start_response.payload {
            ResponsePayload::SessionRegistered {
                ref session,
                session_count,
            } => {
                assert_eq!(session.id, session_id);
                assert_eq!(session.workspace_id, workspace_id);
                assert_eq!(session_count, 1);
            }
            other => panic!("unexpected payload: {other:?}"),
        }

        let list_sessions = RequestEnvelope::query(
            "req-session-list",
            Query::Sessions {
                workspace_id: Some(workspace_id),
            },
        );
        let list_response = handle_request(&state, list_sessions).await;
        match list_response.payload {
            ResponsePayload::Sessions { sessions } => {
                assert_eq!(sessions.len(), 1);
                assert_eq!(sessions[0].id, session_id);
            }
            other => panic!("unexpected payload: {other:?}"),
        }

        let stop_session =
            RequestEnvelope::command("req-session-stop", Command::RemoveSession { session_id });
        let stop_response = handle_request(&state, stop_session).await;
        assert!(matches!(
            stop_response.payload,
            ResponsePayload::SessionRemoved {
                session_id: stopped,
                session_count: 0,
            } if stopped == session_id
        ));

        Ok(())
    }

    #[tokio::test]
    async fn ssh_import_roundtrip_lists_typed_hosts() -> Result<()> {
        let state = Arc::new(DaemonState::new(PathBuf::from("test.sock")));
        let import_response = handle_request(
            &state,
            RequestEnvelope::command(
                "req-ssh-import",
                Command::ImportSshConfig {
                    config_text:
                        "Host alpha beta\n  HostName shared.example\n  User dev\n  Port 2222\n"
                            .into(),
                },
            ),
        )
        .await;

        assert!(matches!(
            import_response.payload,
            ResponsePayload::SshConfigImported { host_count: 2 }
        ));

        let hosts_response = handle_request(
            &state,
            RequestEnvelope::query("req-ssh-hosts", Query::SshHosts),
        )
        .await;

        match hosts_response.payload {
            ResponsePayload::SshHosts { hosts } => {
                assert_eq!(hosts.len(), 2);
                assert_eq!(hosts[0].alias, "alpha");
                assert_eq!(hosts[0].hostname, "shared.example");
                assert_eq!(hosts[0].user.as_deref(), Some("dev"));
                assert_eq!(hosts[0].port, Some(2222));
                assert_eq!(hosts[1].alias, "beta");
            }
            other => panic!("unexpected payload: {other:?}"),
        }

        Ok(())
    }

    #[tokio::test]
    async fn ssh_import_rejects_unsupported_global_directives() -> Result<()> {
        let state = Arc::new(DaemonState::new(PathBuf::from("test.sock")));
        let response = handle_request(
            &state,
            RequestEnvelope::command(
                "req-ssh-import-invalid",
                Command::ImportSshConfig {
                    config_text:
                        "Include ~/.ssh/common.conf\nHost alpha\n  HostName alpha.example\n".into(),
                },
            ),
        )
        .await;

        match response.payload {
            ResponsePayload::Error(error) => {
                assert_eq!(error.code, ErrorCode::ValidationFailed);
                assert!(error.message.contains("unsupported in strict import mode"));
            }
            other => panic!("unexpected payload: {other:?}"),
        }

        Ok(())
    }

    #[tokio::test]
    async fn history_append_and_query_roundtrip() -> Result<()> {
        let state = Arc::new(DaemonState::new(PathBuf::from("test.sock")));
        let workspace_id = Uuid::new_v4();
        let session_id = Uuid::new_v4();

        let append_response = handle_request(
            &state,
            RequestEnvelope::command(
                "req-history-append",
                Command::AppendHistoryEntry {
                    entry_id: Uuid::new_v4(),
                    workspace_id,
                    session_id: Some(session_id),
                    kind: HistoryEntryKind::ChatUser,
                    at_unix_ms: 42,
                    content: "explain the state transition".into(),
                },
            ),
        )
        .await;
        assert!(matches!(
            append_response.payload,
            ResponsePayload::HistoryAppended { history_count: 1 }
        ));

        let recent_response = handle_request(
            &state,
            RequestEnvelope::query(
                "req-history-recent",
                Query::RecentHistory {
                    workspace_id: Some(workspace_id),
                    session_id: Some(session_id),
                    limit: 10,
                },
            ),
        )
        .await;
        match recent_response.payload {
            ResponsePayload::HistoryEntries { entries } => {
                assert_eq!(entries.len(), 1);
                assert_eq!(entries[0].content, "explain the state transition");
            }
            other => panic!("unexpected payload: {other:?}"),
        }

        Ok(())
    }

    #[tokio::test]
    async fn context_search_roundtrip_uses_history_plane() -> Result<()> {
        let state = Arc::new(DaemonState::new(PathBuf::from("test.sock")));
        let workspace_id = Uuid::new_v4();
        let session_id = Uuid::new_v4();

        for (entry_id, kind, at_unix_ms, content) in [
            (
                Uuid::new_v4(),
                HistoryEntryKind::TerminalOutput,
                10_u64,
                "cargo test failed with sqlite busy error",
            ),
            (
                Uuid::new_v4(),
                HistoryEntryKind::ChatAgent,
                20_u64,
                "Try cargo check first, then rerun the sqlite migration path.",
            ),
            (
                Uuid::new_v4(),
                HistoryEntryKind::SystemEvent,
                30_u64,
                "background sync completed",
            ),
        ] {
            let append_response = handle_request(
                &state,
                RequestEnvelope::command(
                    "req-history-append",
                    Command::AppendHistoryEntry {
                        entry_id,
                        workspace_id,
                        session_id: Some(session_id),
                        kind,
                        at_unix_ms,
                        content: content.into(),
                    },
                ),
            )
            .await;
            assert!(matches!(
                append_response.payload,
                ResponsePayload::HistoryAppended { .. }
            ));
        }

        let context_response = handle_request(
            &state,
            RequestEnvelope::query(
                "req-context-search",
                Query::ContextSearch {
                    workspace_id: Some(workspace_id),
                    session_id: Some(session_id),
                    text: "cargo sqlite".into(),
                    limit: 2,
                },
            ),
        )
        .await;

        match context_response.payload {
            ResponsePayload::ContextResults { results } => {
                assert_eq!(results.len(), 2);
                assert!(results[0].score >= results[1].score);
                assert!(results[0].preview.contains("cargo"));
                assert!(results
                    .iter()
                    .all(|result| result.workspace_id == workspace_id));
                assert!(results
                    .iter()
                    .all(|result| result.session_id == Some(session_id)));
            }
            other => panic!("unexpected payload: {other:?}"),
        }

        Ok(())
    }

    #[tokio::test]
    async fn agent_provider_status_and_task_roundtrip() -> Result<()> {
        let script_dir = test_dir("agent-roundtrip");
        let workspace_root = script_dir.join("workspace");
        let session_root = workspace_root.join("session");
        fs::create_dir_all(&workspace_root)?;
        fs::create_dir_all(&session_root)?;
        let script_path = script_dir.join("claw");
        write_fake_claw(
            &script_path,
            r#"#!/bin/sh
case "$*" in
  *version*) printf 'Claw Code\nVersion test\n' ;;
  *doctor*) printf 'Doctor OK\n' ;;
  *status*) printf 'Status OK\n' ;;
  *prompt*)
    printf 'agent-output cwd=%s args=%s\n' "$PWD" "$*"
    ;;
  *)
    printf 'unexpected args: %s\n' "$*" >&2
    exit 1
    ;;
esac
"#,
        );
        let claw = ClawProvider::new(&script_path).expect("fake claw should initialize");
        let state = Arc::new(
            DaemonState::try_new_with_tmux_persistence_secrets_and_claw(
                PathBuf::from("test.sock"),
                TmuxRuntime::system_default(),
                None,
                None,
                Some(claw),
            )
            .expect("daemon state should initialize"),
        );
        let workspace_id = Uuid::new_v4();

        let register_workspace = RequestEnvelope::command(
            "req-workspace",
            Command::RegisterWorkspace {
                workspace_id,
                name: "alpha".into(),
                roots: vec![workspace_root.display().to_string()],
            },
        );
        let workspace_response = handle_request(&state, register_workspace).await;
        assert!(matches!(
            workspace_response.payload,
            ResponsePayload::WorkspaceRegistered { workspace_count: 1 }
        ));

        let session_id = Uuid::new_v4();
        let register_session = handle_request(
            &state,
            RequestEnvelope::command(
                "req-session",
                Command::RegisterSession {
                    session_id,
                    workspace_id,
                    kind: core_domain::SessionKind::Local,
                    backing: core_domain::SessionBacking::LocalPty,
                    cwd: session_root.display().to_string(),
                },
            ),
        )
        .await;
        assert!(matches!(
            register_session.payload,
            ResponsePayload::SessionRegistered { .. }
        ));

        let agent_status = handle_request(
            &state,
            RequestEnvelope::query("req-agent-status", Query::AgentProviderStatus),
        )
        .await;
        match agent_status.payload {
            ResponsePayload::AgentProviderStatus { status } => {
                assert!(status.available);
                assert_eq!(status.provider, "claw");
                assert_eq!(status.doctor_report.as_deref(), Some("Doctor OK"));
            }
            other => panic!("unexpected payload: {other:?}"),
        }

        let run_task = handle_request(
            &state,
            RequestEnvelope::command(
                "req-agent-run",
                Command::RunAgentTask {
                    task_id: Uuid::new_v4(),
                    workspace_id,
                    session_id: Some(session_id),
                    cwd: None,
                    prompt: "summarize recent failures".into(),
                    model: Some("claude-opus-4-6".into()),
                    context_query: Some("recent failures".into()),
                    context_limit: 5,
                    permission_mode: AgentPermissionMode::WorkspaceWrite,
                    allowed_tools: vec!["read".into(), "glob".into()],
                },
            ),
        )
        .await;

        match run_task.payload {
            ResponsePayload::AgentTaskCompleted {
                task,
                output,
                context_result_count,
                history_count,
            } => {
                assert_eq!(task.workspace_id, workspace_id);
                assert_eq!(task.session_id, Some(session_id));
                assert_eq!(task.cwd, session_root);
                assert!(output.contains("agent-output"));
                assert!(output.contains("--permission-mode workspace-write"));
                assert_eq!(context_result_count, 0);
                assert_eq!(history_count, 2);
            }
            other => panic!("unexpected payload: {other:?}"),
        }

        let recent_history = handle_request(
            &state,
            RequestEnvelope::query(
                "req-history",
                Query::RecentHistory {
                    workspace_id: Some(workspace_id),
                    session_id: Some(session_id),
                    limit: 10,
                },
            ),
        )
        .await;

        match recent_history.payload {
            ResponsePayload::HistoryEntries { entries } => {
                assert_eq!(entries.len(), 2);
                assert!(entries
                    .iter()
                    .all(|entry| entry.session_id == Some(session_id)));
                assert!(entries
                    .iter()
                    .any(|entry| entry.kind == HistoryEntryKind::ChatUser));
                assert!(entries
                    .iter()
                    .any(|entry| entry.kind == HistoryEntryKind::ChatAgent));
            }
            other => panic!("unexpected payload: {other:?}"),
        }

        fs::remove_dir_all(script_dir)?;
        Ok(())
    }

    #[tokio::test]
    async fn agent_policy_denies_danger_full_access_roundtrip() -> Result<()> {
        let workspace_root = test_dir("agent-policy-deny");
        fs::create_dir_all(&workspace_root)?;
        let state = Arc::new(DaemonState::new(PathBuf::from("test.sock")));
        let workspace_id = Uuid::new_v4();

        let workspace_response = handle_request(
            &state,
            RequestEnvelope::command(
                "req-workspace",
                Command::RegisterWorkspace {
                    workspace_id,
                    name: "alpha".into(),
                    roots: vec![workspace_root.display().to_string()],
                },
            ),
        )
        .await;
        assert!(matches!(
            workspace_response.payload,
            ResponsePayload::WorkspaceRegistered { workspace_count: 1 }
        ));

        let denied = handle_request(
            &state,
            RequestEnvelope::command(
                "req-agent-run",
                Command::RunAgentTask {
                    task_id: Uuid::new_v4(),
                    workspace_id,
                    session_id: None,
                    cwd: Some(workspace_root.display().to_string()),
                    prompt: "try dangerous mode".into(),
                    model: None,
                    context_query: None,
                    context_limit: 5,
                    permission_mode: AgentPermissionMode::DangerFullAccess,
                    allowed_tools: Vec::new(),
                },
            ),
        )
        .await;

        match denied.payload {
            ResponsePayload::Error(error) => {
                assert_eq!(error.code, ErrorCode::ValidationFailed);
                assert!(error.message.contains("danger-full-access"));
            }
            other => panic!("unexpected payload: {other:?}"),
        }

        let _ = fs::remove_dir_all(workspace_root);
        Ok(())
    }

    #[tokio::test]
    async fn agent_task_rejects_dotdot_escape_outside_workspace() -> Result<()> {
        let root_dir = test_dir("agent-cwd-escape");
        let workspace_root = root_dir.join("workspace");
        let outside_root = root_dir.join("outside");
        fs::create_dir_all(&workspace_root)?;
        fs::create_dir_all(&outside_root)?;

        let state = Arc::new(DaemonState::new(PathBuf::from("test.sock")));
        let workspace_id = Uuid::new_v4();

        let workspace_response = handle_request(
            &state,
            RequestEnvelope::command(
                "req-workspace",
                Command::RegisterWorkspace {
                    workspace_id,
                    name: "alpha".into(),
                    roots: vec![workspace_root.display().to_string()],
                },
            ),
        )
        .await;
        assert!(matches!(
            workspace_response.payload,
            ResponsePayload::WorkspaceRegistered { workspace_count: 1 }
        ));

        let escaped_cwd = workspace_root.join("../outside");
        let build_error = state
            .build_agent_task(&AgentExecutionRequest {
                task_id: Uuid::new_v4(),
                workspace_id,
                session_id: None,
                cwd: Some(escaped_cwd),
                prompt: "stay inside workspace".into(),
                model: None,
                context_query: None,
                context_limit: 3,
                permission_mode: AgentPermissionMode::ReadOnly,
                allowed_tools: vec!["read".into()],
            })
            .await
            .expect_err("path traversal cwd must be denied");

        assert_eq!(build_error.code, ErrorCode::ValidationFailed);
        assert!(build_error.message.contains("outside registered workspace roots"));

        let _ = fs::remove_dir_all(root_dir);
        Ok(())
    }

    #[tokio::test]
    async fn agent_task_rejects_session_from_other_workspace() -> Result<()> {
        let root_dir = test_dir("agent-session-scope");
        let workspace_a_root = root_dir.join("workspace-a");
        let workspace_b_root = root_dir.join("workspace-b");
        fs::create_dir_all(&workspace_a_root)?;
        fs::create_dir_all(&workspace_b_root)?;

        let state = Arc::new(DaemonState::new(PathBuf::from("test.sock")));
        let workspace_a = Uuid::new_v4();
        let workspace_b = Uuid::new_v4();
        let session_id = Uuid::new_v4();

        for (workspace_id, name, root) in [
            (workspace_a, "alpha", &workspace_a_root),
            (workspace_b, "beta", &workspace_b_root),
        ] {
            let response = handle_request(
                &state,
                RequestEnvelope::command(
                    format!("req-workspace-{name}"),
                    Command::RegisterWorkspace {
                        workspace_id,
                        name: name.into(),
                        roots: vec![root.display().to_string()],
                    },
                ),
            )
            .await;
            assert!(matches!(
                response.payload,
                ResponsePayload::WorkspaceRegistered { .. }
            ));
        }

        let session_response = handle_request(
            &state,
            RequestEnvelope::command(
                "req-session",
                Command::RegisterSession {
                    session_id,
                    workspace_id: workspace_a,
                    kind: core_domain::SessionKind::Local,
                    backing: core_domain::SessionBacking::LocalPty,
                    cwd: workspace_a_root.display().to_string(),
                },
            ),
        )
        .await;
        assert!(matches!(
            session_response.payload,
            ResponsePayload::SessionRegistered { .. }
        ));

        let error = state
            .build_agent_task(&AgentExecutionRequest {
                task_id: Uuid::new_v4(),
                workspace_id: workspace_b,
                session_id: Some(session_id),
                cwd: None,
                prompt: "should fail".into(),
                model: None,
                context_query: None,
                context_limit: 3,
                permission_mode: AgentPermissionMode::ReadOnly,
                allowed_tools: vec!["read".into()],
            })
            .await
            .expect_err("cross-workspace session must be denied");

        assert_eq!(error.code, ErrorCode::ValidationFailed);
        assert!(error.message.contains("does not belong to workspace"));

        let _ = fs::remove_dir_all(root_dir);
        Ok(())
    }

    #[tokio::test]
    async fn in_memory_history_retention_is_bounded() -> Result<()> {
        let state = Arc::new(DaemonState::new(PathBuf::from("test.sock")));
        let workspace_id = Uuid::new_v4();

        for offset in 0..(MAX_IN_MEMORY_HISTORY_ENTRIES + 32) {
            state.append_history_entry(
                HistoryEntry::new(
                    Uuid::new_v4(),
                    workspace_id,
                    None,
                    HistoryEntryKind::TerminalOutput,
                    offset as u64,
                    format!("event-{offset}"),
                )?,
            )
            .await?;
        }

        let recent = state
            .recent_history(None, None, MAX_IN_MEMORY_HISTORY_ENTRIES + 64)
            .await;
        assert_eq!(recent.len(), MAX_IN_MEMORY_HISTORY_ENTRIES);
        assert_eq!(recent.first().map(|entry| entry.content.as_str()), Some("event-2079"));
        assert_eq!(
            recent.last().map(|entry| entry.content.as_str()),
            Some("event-32")
        );

        Ok(())
    }

    #[tokio::test]
    async fn persistent_state_reloads_after_restart() -> Result<()> {
        let db_path = test_state_db_path("daemon-state");
        if let Some(parent) = db_path.parent() {
            std::fs::create_dir_all(parent)?;
        }

        let persistence = SqliteStateStore::open(&db_path)?;
        let workspace_id = Uuid::new_v4();
        let session_id = Uuid::new_v4();

        let state = Arc::new(DaemonState::try_new_with_persistence(
            PathBuf::from("test.sock"),
            persistence.clone(),
        )?);

        let workspace_response = handle_request(
            &state,
            RequestEnvelope::command(
                "req-workspace",
                Command::RegisterWorkspace {
                    workspace_id,
                    name: "alpha".into(),
                    roots: vec![env!("CARGO_MANIFEST_DIR").into()],
                },
            ),
        )
        .await;
        assert!(matches!(
            workspace_response.payload,
            ResponsePayload::WorkspaceRegistered { workspace_count: 1 }
        ));

        let session_response = handle_request(
            &state,
            RequestEnvelope::command(
                "req-session",
                Command::RegisterSession {
                    session_id,
                    workspace_id,
                    kind: core_domain::SessionKind::Local,
                    backing: core_domain::SessionBacking::LocalPty,
                    cwd: "/workspace".into(),
                },
            ),
        )
        .await;
        assert!(matches!(
            session_response.payload,
            ResponsePayload::SessionRegistered {
                session_count: 1,
                ..
            }
        ));

        drop(state);

        let reloaded = Arc::new(DaemonState::try_new_with_persistence(
            PathBuf::from("test.sock"),
            persistence,
        )?);

        let workspace_count_response = handle_request(
            &reloaded,
            RequestEnvelope::query("req-workspace-count", Query::WorkspaceCount),
        )
        .await;
        assert!(matches!(
            workspace_count_response.payload,
            ResponsePayload::WorkspaceCount { workspace_count: 1 }
        ));

        let sessions_response = handle_request(
            &reloaded,
            RequestEnvelope::query(
                "req-sessions",
                Query::Sessions {
                    workspace_id: Some(workspace_id),
                },
            ),
        )
        .await;
        match sessions_response.payload {
            ResponsePayload::Sessions { sessions } => {
                assert_eq!(sessions.len(), 1);
                assert_eq!(sessions[0].id, session_id);
            }
            other => panic!("unexpected payload: {other:?}"),
        }

        let _ = std::fs::remove_file(db_path);
        Ok(())
    }

    #[tokio::test]
    async fn persistent_history_reloads_after_restart() -> Result<()> {
        let state_db_path = test_state_db_path("daemon-state-history");
        let history_db_path = state_db_path.with_extension("history.sqlite3");
        if let Some(parent) = state_db_path.parent() {
            std::fs::create_dir_all(parent)?;
        }

        let persistence = SqliteStateStore::open(&state_db_path)?;
        let workspace_id = Uuid::new_v4();
        let session_id = Uuid::new_v4();

        let result = async {
            let state = Arc::new(DaemonState::try_new_with_persistence(
                PathBuf::from("test.sock"),
                persistence.clone(),
            )?);

            let append_response = handle_request(
                &state,
                RequestEnvelope::command(
                    "req-history-append",
                    Command::AppendHistoryEntry {
                        entry_id: Uuid::new_v4(),
                        workspace_id,
                        session_id: Some(session_id),
                        kind: HistoryEntryKind::TerminalOutput,
                        at_unix_ms: 100,
                        content: "cargo test --workspace".into(),
                    },
                ),
            )
            .await;
            assert!(matches!(
                append_response.payload,
                ResponsePayload::HistoryAppended { history_count: 1 }
            ));

            drop(state);

            let reloaded = Arc::new(DaemonState::try_new_with_persistence(
                PathBuf::from("test.sock"),
                persistence,
            )?);

            let history_response = handle_request(
                &reloaded,
                RequestEnvelope::query(
                    "req-history-recent",
                    Query::RecentHistory {
                        workspace_id: Some(workspace_id),
                        session_id: Some(session_id),
                        limit: 10,
                    },
                ),
            )
            .await;
            match history_response.payload {
                ResponsePayload::HistoryEntries { entries } => {
                    assert_eq!(entries.len(), 1);
                    assert_eq!(entries[0].content, "cargo test --workspace");
                }
                other => panic!("unexpected payload: {other:?}"),
            }

            Result::<()>::Ok(())
        }
        .await;

        let _ = std::fs::remove_file(state_db_path);
        let _ = std::fs::remove_file(history_db_path);

        result
    }

    #[tokio::test]
    async fn persistent_context_reloads_from_history_after_restart() -> Result<()> {
        let state_db_path = test_state_db_path("daemon-state-context");
        let history_db_path = state_db_path.with_extension("history.sqlite3");
        if let Some(parent) = state_db_path.parent() {
            std::fs::create_dir_all(parent)?;
        }

        let persistence = SqliteStateStore::open(&state_db_path)?;
        let workspace_id = Uuid::new_v4();
        let session_id = Uuid::new_v4();

        let result = async {
            let state = Arc::new(DaemonState::try_new_with_persistence(
                PathBuf::from("test.sock"),
                persistence.clone(),
            )?);

            let append_response = handle_request(
                &state,
                RequestEnvelope::command(
                    "req-history-append",
                    Command::AppendHistoryEntry {
                        entry_id: Uuid::new_v4(),
                        workspace_id,
                        session_id: Some(session_id),
                        kind: HistoryEntryKind::ChatAgent,
                        at_unix_ms: 100,
                        content: "cargo test failed because sqlite journal was locked".into(),
                    },
                ),
            )
            .await;
            assert!(matches!(
                append_response.payload,
                ResponsePayload::HistoryAppended { history_count: 1 }
            ));

            drop(state);

            let reloaded = Arc::new(DaemonState::try_new_with_persistence(
                PathBuf::from("test.sock"),
                persistence,
            )?);

            let context_response = handle_request(
                &reloaded,
                RequestEnvelope::query(
                    "req-context-search",
                    Query::ContextSearch {
                        workspace_id: Some(workspace_id),
                        session_id: Some(session_id),
                        text: "sqlite locked".into(),
                        limit: 5,
                    },
                ),
            )
            .await;
            match context_response.payload {
                ResponsePayload::ContextResults { results } => {
                    assert_eq!(results.len(), 1);
                    assert!(results[0].preview.contains("sqlite"));
                }
                other => panic!("unexpected payload: {other:?}"),
            }

            Result::<()>::Ok(())
        }
        .await;

        let _ = std::fs::remove_file(state_db_path);
        let _ = std::fs::remove_file(history_db_path);

        result
    }

    #[tokio::test]
    async fn recovery_bundle_export_import_roundtrip() -> Result<()> {
        let bundle_path = test_recovery_bundle_path("daemon-recovery");
        let state = Arc::new(DaemonState::new_with_secrets(
            PathBuf::from("test.sock"),
            Some(SecretsStore::from_passphrase(SecretString::from(
                "test-passphrase",
            ))?),
        ));
        let workspace_id = Uuid::new_v4();
        let session_id = Uuid::new_v4();

        let workspace_response = handle_request(
            &state,
            RequestEnvelope::command(
                "req-workspace",
                Command::RegisterWorkspace {
                    workspace_id,
                    name: "alpha".into(),
                    roots: vec![env!("CARGO_MANIFEST_DIR").into()],
                },
            ),
        )
        .await;
        assert!(matches!(
            workspace_response.payload,
            ResponsePayload::WorkspaceRegistered { workspace_count: 1 }
        ));

        let session_response = handle_request(
            &state,
            RequestEnvelope::command(
                "req-session",
                Command::RegisterSession {
                    session_id,
                    workspace_id,
                    kind: core_domain::SessionKind::Local,
                    backing: core_domain::SessionBacking::LocalPty,
                    cwd: "/workspace".into(),
                },
            ),
        )
        .await;
        assert!(matches!(
            session_response.payload,
            ResponsePayload::SessionRegistered {
                session_count: 1,
                ..
            }
        ));

        let history_response = handle_request(
            &state,
            RequestEnvelope::command(
                "req-history",
                Command::AppendHistoryEntry {
                    entry_id: Uuid::new_v4(),
                    workspace_id,
                    session_id: Some(session_id),
                    kind: HistoryEntryKind::ChatAgent,
                    at_unix_ms: 42,
                    content: "cargo test failed".into(),
                },
            ),
        )
        .await;
        assert!(matches!(
            history_response.payload,
            ResponsePayload::HistoryAppended { history_count: 1 }
        ));

        let ssh_response = handle_request(
            &state,
            RequestEnvelope::command(
                "req-ssh-import",
                Command::ImportSshConfig {
                    config_text: "Host alpha\n  HostName alpha.example\n  User dev\n".into(),
                },
            ),
        )
        .await;
        assert!(matches!(
            ssh_response.payload,
            ResponsePayload::SshConfigImported { host_count: 1 }
        ));

        let export_response = handle_request(
            &state,
            RequestEnvelope::command(
                "req-export",
                Command::ExportRecoveryBundle {
                    bundle_path: bundle_path.display().to_string(),
                },
            ),
        )
        .await;
        assert!(matches!(
            export_response.payload,
            ResponsePayload::RecoveryBundleExported {
                workspace_count: 1,
                session_count: 1,
                history_count: 1,
                ssh_host_count: 1,
                ..
            }
        ));

        let restored = Arc::new(DaemonState::new_with_secrets(
            PathBuf::from("restored.sock"),
            Some(SecretsStore::from_passphrase(SecretString::from(
                "test-passphrase",
            ))?),
        ));
        let import_response = handle_request(
            &restored,
            RequestEnvelope::command(
                "req-import",
                Command::ImportRecoveryBundle {
                    bundle_path: bundle_path.display().to_string(),
                },
            ),
        )
        .await;
        assert!(matches!(
            import_response.payload,
            ResponsePayload::RecoveryBundleImported {
                workspace_count: 1,
                session_count: 1,
                history_count: 1,
                ssh_host_count: 1,
                ..
            }
        ));

        let sessions_response = handle_request(
            &restored,
            RequestEnvelope::query(
                "req-sessions",
                Query::Sessions {
                    workspace_id: Some(workspace_id),
                },
            ),
        )
        .await;
        assert!(matches!(
            sessions_response.payload,
            ResponsePayload::Sessions { ref sessions } if sessions.len() == 1
        ));

        let context_response = handle_request(
            &restored,
            RequestEnvelope::query(
                "req-context",
                Query::ContextSearch {
                    workspace_id: Some(workspace_id),
                    session_id: Some(session_id),
                    text: "cargo failed".into(),
                    limit: 5,
                },
            ),
        )
        .await;
        assert!(matches!(
            context_response.payload,
            ResponsePayload::ContextResults { ref results } if results.len() == 1
        ));

        let ssh_hosts_response = handle_request(
            &restored,
            RequestEnvelope::query("req-ssh-hosts", Query::SshHosts),
        )
        .await;
        assert!(matches!(
            ssh_hosts_response.payload,
            ResponsePayload::SshHosts { ref hosts } if hosts.len() == 1
        ));

        let _ = std::fs::remove_file(bundle_path);
        Ok(())
    }

    #[tokio::test]
    async fn recovery_bundle_rekey_roundtrip() -> Result<()> {
        let input_path = test_recovery_bundle_path("daemon-recovery-input");
        let output_path = test_recovery_bundle_path("daemon-recovery-output");
        let state = Arc::new(DaemonState::new_with_secrets(
            PathBuf::from("test.sock"),
            Some(SecretsStore::from_passphrase(SecretString::from(
                "current-passphrase",
            ))?),
        ));
        let workspace_id = Uuid::new_v4();

        let workspace_response = handle_request(
            &state,
            RequestEnvelope::command(
                "req-workspace",
                Command::RegisterWorkspace {
                    workspace_id,
                    name: "alpha".into(),
                    roots: vec![env!("CARGO_MANIFEST_DIR").into()],
                },
            ),
        )
        .await;
        assert!(matches!(
            workspace_response.payload,
            ResponsePayload::WorkspaceRegistered { workspace_count: 1 }
        ));

        let export_response = handle_request(
            &state,
            RequestEnvelope::command(
                "req-export",
                Command::ExportRecoveryBundle {
                    bundle_path: input_path.display().to_string(),
                },
            ),
        )
        .await;
        assert!(matches!(
            export_response.payload,
            ResponsePayload::RecoveryBundleExported { .. }
        ));

        let rekey_response = handle_request(
            &state,
            RequestEnvelope::command(
                "req-rekey",
                Command::RekeyRecoveryBundle {
                    input_path: input_path.display().to_string(),
                    output_path: output_path.display().to_string(),
                    new_passphrase: "next-passphrase".into(),
                },
            ),
        )
        .await;
        assert!(matches!(
            rekey_response.payload,
            ResponsePayload::RecoveryBundleRekeyed {
                workspace_count: 1,
                ..
            }
        ));

        let restored = Arc::new(DaemonState::new_with_secrets(
            PathBuf::from("restored.sock"),
            Some(SecretsStore::from_passphrase(SecretString::from(
                "next-passphrase",
            ))?),
        ));
        let import_response = handle_request(
            &restored,
            RequestEnvelope::command(
                "req-import",
                Command::ImportRecoveryBundle {
                    bundle_path: output_path.display().to_string(),
                },
            ),
        )
        .await;
        assert!(matches!(
            import_response.payload,
            ResponsePayload::RecoveryBundleImported {
                workspace_count: 1,
                ..
            }
        ));

        let _ = std::fs::remove_file(input_path);
        let _ = std::fs::remove_file(output_path);
        Ok(())
    }

    #[tokio::test]
    async fn file_index_roundtrip_lists_previews_and_searches() -> Result<()> {
        let root = test_file_index_root("daemon-file-index");
        let workspace_id = Uuid::new_v4();
        std::fs::create_dir_all(root.join("src"))?;
        std::fs::write(root.join("src/lib.rs"), "pub fn cargo_test() {}\n")?;
        std::fs::write(root.join("README.md"), "cargo test guidance\n")?;

        let state = Arc::new(DaemonState::new(PathBuf::from("test.sock")));
        let refresh_response = handle_request(
            &state,
            RequestEnvelope::command(
                "req-workspace",
                Command::RegisterWorkspace {
                    workspace_id,
                    name: "alpha".into(),
                    roots: vec![root.display().to_string()],
                },
            ),
        )
        .await;
        assert!(matches!(
            refresh_response.payload,
            ResponsePayload::WorkspaceRegistered { workspace_count: 1 }
        ));

        let refresh_response = handle_request(
            &state,
            RequestEnvelope::command(
                "req-file-refresh",
                Command::RefreshFileIndex {
                    workspace_id,
                    root_path: root.display().to_string(),
                },
            ),
        )
        .await;
        assert!(matches!(
            refresh_response.payload,
            ResponsePayload::FileIndexRefreshed { indexed_count, .. } if indexed_count >= 2
        ));

        let list_response = handle_request(
            &state,
            RequestEnvelope::query(
                "req-file-list",
                Query::FileList {
                    workspace_id,
                    path: root.display().to_string(),
                    limit: 10,
                },
            ),
        )
        .await;
        assert!(matches!(
            list_response.payload,
            ResponsePayload::FileEntries { ref entries } if entries.len() >= 2
        ));

        let preview_response = handle_request(
            &state,
            RequestEnvelope::query(
                "req-file-preview",
                Query::FilePreview {
                    workspace_id,
                    path: root.join("README.md").display().to_string(),
                    max_bytes: 512,
                    max_lines: 10,
                },
            ),
        )
        .await;
        assert!(matches!(
            preview_response.payload,
            ResponsePayload::FilePreview { ref preview }
                if preview.content.as_deref() == Some("cargo test guidance")
        ));

        let search_response = handle_request(
            &state,
            RequestEnvelope::query(
                "req-file-search",
                Query::FileSearch {
                    workspace_id,
                    root_path: root.display().to_string(),
                    text: "cargo readme".into(),
                    limit: 5,
                },
            ),
        )
        .await;
        assert!(matches!(
            search_response.payload,
            ResponsePayload::FileSearchResults { ref results }
                if !results.is_empty() && results[0].entry.path.ends_with("README.md")
        ));

        let _ = std::fs::remove_dir_all(root);
        Ok(())
    }

    #[tokio::test]
    async fn file_queries_are_denied_across_workspace_boundaries() -> Result<()> {
        let root_a = test_file_index_root("daemon-file-scope-a");
        let root_b = test_file_index_root("daemon-file-scope-b");
        std::fs::create_dir_all(&root_a)?;
        std::fs::create_dir_all(&root_b)?;
        std::fs::write(root_a.join("secret.txt"), "alpha")?;

        let state = Arc::new(DaemonState::new(PathBuf::from("test.sock")));
        let workspace_a = Uuid::new_v4();
        let workspace_b = Uuid::new_v4();

        for (workspace_id, root) in [(workspace_a, &root_a), (workspace_b, &root_b)] {
            let response = handle_request(
                &state,
                RequestEnvelope::command(
                    "req-workspace",
                    Command::RegisterWorkspace {
                        workspace_id,
                        name: format!("ws-{workspace_id}"),
                        roots: vec![root.display().to_string()],
                    },
                ),
            )
            .await;
            assert!(matches!(
                response.payload,
                ResponsePayload::WorkspaceRegistered { .. }
            ));
        }

        let denied = handle_request(
            &state,
            RequestEnvelope::query(
                "req-file-list",
                Query::FileList {
                    workspace_id: workspace_b,
                    path: root_a.display().to_string(),
                    limit: 10,
                },
            ),
        )
        .await;

        match denied.payload {
            ResponsePayload::Error(error) => {
                assert_eq!(error.code, ErrorCode::ValidationFailed);
                assert!(error.message.contains("outside registered workspace roots"));
            }
            other => panic!("unexpected payload: {other:?}"),
        }

        let _ = std::fs::remove_dir_all(root_a);
        let _ = std::fs::remove_dir_all(root_b);
        Ok(())
    }

    #[tokio::test]
    async fn local_tmux_session_roundtrip_uses_real_tmux_runtime() -> Result<()> {
        let socket_path = test_tmux_socket_path("daemon");
        if let Some(parent) = socket_path.parent() {
            std::fs::create_dir_all(parent)?;
        }

        let tmux = TmuxRuntime::isolated(&socket_path);
        let state = Arc::new(DaemonState::new_with_tmux(
            PathBuf::from("test.sock"),
            tmux.clone(),
        ));
        let workspace_id = Uuid::new_v4();
        let session_id = Uuid::new_v4();

        let register_workspace = RequestEnvelope::command(
            "req-workspace",
            Command::RegisterWorkspace {
                workspace_id,
                name: "alpha".into(),
                roots: vec![env!("CARGO_MANIFEST_DIR").into()],
            },
        );
        let workspace_response = handle_request(&state, register_workspace).await;
        assert!(matches!(
            workspace_response.payload,
            ResponsePayload::WorkspaceRegistered { workspace_count: 1 }
        ));

        let register_tmux = RequestEnvelope::command(
            "req-tmux",
            Command::RegisterLocalTmuxSession {
                session_id,
                workspace_id,
                cwd: env!("CARGO_MANIFEST_DIR").into(),
            },
        );
        let tmux_response = handle_request(&state, register_tmux).await;
        match tmux_response.payload {
            ResponsePayload::SessionRegistered {
                ref session,
                session_count,
            } => {
                assert_eq!(session.id, session_id);
                assert_eq!(session.kind, core_domain::SessionKind::Tmux);
                assert_eq!(session.backing, core_domain::SessionBacking::TmuxSession);
                assert_eq!(session_count, 1);
            }
            other => panic!("unexpected payload: {other:?}"),
        }

        let sessions_response = handle_request(
            &state,
            RequestEnvelope::query(
                "req-sessions",
                Query::Sessions {
                    workspace_id: Some(workspace_id),
                },
            ),
        )
        .await;
        match sessions_response.payload {
            ResponsePayload::Sessions { sessions } => {
                assert_eq!(sessions.len(), 1);
                assert_eq!(sessions[0].id, session_id);
            }
            other => panic!("unexpected payload: {other:?}"),
        }

        let remove_response = handle_request(
            &state,
            RequestEnvelope::command("req-remove", Command::RemoveSession { session_id }),
        )
        .await;
        assert!(matches!(
            remove_response.payload,
            ResponsePayload::SessionRemoved {
                session_count: 0,
                ..
            }
        ));
        assert!(tmux.list_sessions()?.is_empty());

        tmux.kill_server()?;
        Ok(())
    }

    #[tokio::test]
    async fn duplicate_tmux_registration_does_not_create_orphaned_session() -> Result<()> {
        let socket_path = test_tmux_socket_path("daemon-duplicate");
        if let Some(parent) = socket_path.parent() {
            std::fs::create_dir_all(parent)?;
        }

        let tmux = TmuxRuntime::isolated(&socket_path);
        let state = Arc::new(DaemonState::new_with_tmux(
            PathBuf::from("test.sock"),
            tmux.clone(),
        ));
        let workspace_id = Uuid::new_v4();
        let session_id = Uuid::new_v4();

        let workspace_response = handle_request(
            &state,
            RequestEnvelope::command(
                "req-workspace",
                Command::RegisterWorkspace {
                    workspace_id,
                    name: "alpha".into(),
                    roots: vec![env!("CARGO_MANIFEST_DIR").into()],
                },
            ),
        )
        .await;
        assert!(matches!(
            workspace_response.payload,
            ResponsePayload::WorkspaceRegistered { workspace_count: 1 }
        ));

        let local_response = handle_request(
            &state,
            RequestEnvelope::command(
                "req-local",
                Command::RegisterSession {
                    session_id,
                    workspace_id,
                    kind: core_domain::SessionKind::Local,
                    backing: core_domain::SessionBacking::LocalPty,
                    cwd: env!("CARGO_MANIFEST_DIR").into(),
                },
            ),
        )
        .await;
        assert!(matches!(
            local_response.payload,
            ResponsePayload::SessionRegistered {
                session_count: 1,
                ..
            }
        ));

        let duplicate_tmux_response = handle_request(
            &state,
            RequestEnvelope::command(
                "req-tmux-duplicate",
                Command::RegisterLocalTmuxSession {
                    session_id,
                    workspace_id,
                    cwd: env!("CARGO_MANIFEST_DIR").into(),
                },
            ),
        )
        .await;
        assert!(matches!(
            duplicate_tmux_response.payload,
            ResponsePayload::Error(_)
        ));

        assert!(tmux.list_sessions()?.is_empty());
        tmux.kill_server()?;
        Ok(())
    }

    #[tokio::test]
    #[ignore = "sandbox blocks UnixListener::bind; run on host to validate end-to-end uds transport"]
    async fn uds_roundtrip_serves_ping_and_health() -> Result<()> {
        let socket_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(".tmp");
        std::fs::create_dir_all(&socket_dir)?;
        let socket_path = socket_dir.join(format!("uds-{}.sock", &Uuid::new_v4().to_string()[..8]));
        let listener = bind_listener(&socket_path).await?;
        let state = Arc::new(DaemonState::new(socket_path.clone()));

        let server = tokio::spawn(run_daemon(listener, Arc::clone(&state)));

        let result = async {
            let stream = UnixStream::connect(&socket_path).await?;
            let (reader, mut writer) = stream.into_split();
            let mut lines = BufReader::new(reader).lines();

            let ping = serde_json::to_string(&RequestEnvelope::command("req-ping", Command::Ping))?;
            writer.write_all(ping.as_bytes()).await?;
            writer.write_all(b"\n").await?;
            writer.flush().await?;

            let ping_response = lines.next_line().await?.expect("ping response line");
            let ping_envelope: ResponseEnvelope = serde_json::from_str(&ping_response)?;
            assert!(matches!(ping_envelope.payload, ResponsePayload::Pong));

            let health =
                serde_json::to_string(&RequestEnvelope::query("req-health", Query::Health))?;
            writer.write_all(health.as_bytes()).await?;
            writer.write_all(b"\n").await?;
            writer.flush().await?;

            let health_response = lines.next_line().await?.expect("health response line");
            let health_envelope: ResponseEnvelope = serde_json::from_str(&health_response)?;
            match health_envelope.payload {
                ResponsePayload::Health(snapshot) => {
                    assert_eq!(snapshot.workspace_count, 0);
                    assert_eq!(snapshot.session_count, 0);
                    assert_eq!(
                        snapshot.daemon.socket_path,
                        socket_path.display().to_string()
                    );
                }
                other => panic!("unexpected payload: {other:?}"),
            }

            Result::<()>::Ok(())
        }
        .await;

        server.abort();
        let _ = server.await;

        if socket_path.exists() {
            std::fs::remove_file(&socket_path)?;
        }

        result
    }

    fn test_tmux_socket_path(prefix: &str) -> PathBuf {
        let stamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("time should move forward")
            .as_nanos();

        PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join(".tmp")
            .join(format!("{prefix}-{stamp}.sock"))
    }

    fn test_state_db_path(prefix: &str) -> PathBuf {
        let stamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("time should move forward")
            .as_nanos();

        PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join(".tmp")
            .join(format!("{prefix}-{stamp}.sqlite3"))
    }

    fn test_recovery_bundle_path(prefix: &str) -> PathBuf {
        let stamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("time should move forward")
            .as_nanos();

        PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join(".tmp")
            .join(format!("{prefix}-{stamp}.edex-recovery"))
    }

    fn test_file_index_root(prefix: &str) -> PathBuf {
        let stamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("time should move forward")
            .as_nanos();

        std::env::temp_dir().join(format!("{prefix}-{stamp}"))
    }

    fn test_dir(label: &str) -> PathBuf {
        let path = std::env::temp_dir().join(format!(
            "edex-ui-2026-runtime-daemon-{label}-{}",
            Uuid::new_v4()
        ));
        fs::create_dir_all(&path).expect("test dir should exist");
        path
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

use core_domain::{DomainError, Session, SessionId, Workspace, WorkspaceId};
use rusqlite::{params, Connection};
use secrets_store::{SecretsStore, SecretsStoreError};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};
use uuid::Uuid;

#[derive(Debug, Default, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct StateSnapshot {
    pub daemon_id: Uuid,
    pub started_at_unix_ms: u64,
    pub last_ping_at_unix_ms: Option<u64>,
    pub schema_version: u32,
    pub revision: u64,
    pub workspaces: HashMap<WorkspaceId, Workspace>,
    pub sessions: HashMap<SessionId, Session>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct PingReceipt {
    pub daemon_id: Uuid,
    pub at_unix_ms: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct HealthSnapshot {
    pub daemon_id: Uuid,
    pub schema_version: u32,
    pub started_at_unix_ms: u64,
    pub last_ping_at_unix_ms: Option<u64>,
    pub workspace_count: usize,
    pub session_count: usize,
    pub ready: bool,
}

#[derive(Debug, Clone)]
pub struct CanonicalStore {
    daemon_id: Uuid,
    schema_version: u32,
    revision: u64,
    started_at: SystemTime,
    last_ping_at: Option<SystemTime>,
    workspaces: HashMap<WorkspaceId, Workspace>,
    sessions: HashMap<SessionId, Session>,
}

#[derive(Debug, Clone)]
pub struct SqliteStateStore {
    db_path: PathBuf,
    secrets: Option<SecretsStore>,
}

impl Default for CanonicalStore {
    fn default() -> Self {
        Self::new_in_memory()
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorkspaceRegistration {
    pub workspace: Workspace,
    pub inserted: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionRegistration {
    pub session: Session,
    pub inserted: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionStop {
    pub session_id: SessionId,
    pub removed: bool,
}

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum StateError {
    #[error(transparent)]
    Domain(#[from] DomainError),
    #[error("workspace root `{0}` is not accessible")]
    InvalidWorkspaceRoot(String),
    #[error("state persistence error: {0}")]
    Storage(String),
    #[error("workspace `{0}` is already registered")]
    WorkspaceAlreadyExists(WorkspaceId),
    #[error("workspace `{0}` is not registered")]
    WorkspaceNotFound(WorkspaceId),
    #[error("session `{0}` is already registered")]
    SessionAlreadyExists(SessionId),
    #[error("session `{0}` is not registered")]
    SessionNotFound(SessionId),
}

impl CanonicalStore {
    pub fn new_in_memory() -> Self {
        Self {
            daemon_id: Uuid::new_v4(),
            schema_version: 1,
            revision: 0,
            started_at: SystemTime::now(),
            last_ping_at: None,
            workspaces: HashMap::new(),
            sessions: HashMap::new(),
        }
    }

    pub fn ping(&mut self) -> PingReceipt {
        let now = SystemTime::now();
        self.last_ping_at = Some(now);
        self.revision += 1;

        PingReceipt {
            daemon_id: self.daemon_id,
            at_unix_ms: unix_ms(now),
        }
    }

    pub fn health_snapshot(&self) -> HealthSnapshot {
        HealthSnapshot {
            daemon_id: self.daemon_id,
            schema_version: self.schema_version,
            started_at_unix_ms: unix_ms(self.started_at),
            last_ping_at_unix_ms: self.last_ping_at.map(unix_ms),
            workspace_count: self.workspaces.len(),
            session_count: self.sessions.len(),
            ready: true,
        }
    }

    pub fn register_workspace(
        &mut self,
        id: WorkspaceId,
        name: impl Into<String>,
        roots: Vec<PathBuf>,
    ) -> Result<WorkspaceRegistration, StateError> {
        if self.workspaces.contains_key(&id) {
            return Err(StateError::WorkspaceAlreadyExists(id));
        }

        let workspace = Workspace::new(id, name)?.with_roots(roots)?;
        self.workspaces.insert(id, workspace.clone());
        self.revision += 1;

        Ok(WorkspaceRegistration {
            workspace,
            inserted: true,
        })
    }

    pub fn snapshot(&self) -> StateSnapshot {
        StateSnapshot {
            daemon_id: self.daemon_id,
            started_at_unix_ms: unix_ms(self.started_at),
            last_ping_at_unix_ms: self.last_ping_at.map(unix_ms),
            schema_version: self.schema_version,
            revision: self.revision,
            workspaces: self.workspaces.clone(),
            sessions: self.sessions.clone(),
        }
    }

    pub fn from_snapshot(snapshot: StateSnapshot) -> Self {
        Self {
            daemon_id: snapshot.daemon_id,
            schema_version: snapshot.schema_version,
            revision: snapshot.revision,
            started_at: time_from_unix_ms(snapshot.started_at_unix_ms),
            last_ping_at: snapshot.last_ping_at_unix_ms.map(time_from_unix_ms),
            workspaces: snapshot.workspaces,
            sessions: snapshot.sessions,
        }
    }

    pub fn workspace(&self, id: WorkspaceId) -> Option<&Workspace> {
        self.workspaces.get(&id)
    }

    pub fn workspace_count(&self) -> usize {
        self.workspaces.len()
    }

    pub fn workspaces(&self) -> Vec<Workspace> {
        let mut workspaces: Vec<_> = self.workspaces.values().cloned().collect();
        workspaces.sort_by(|left, right| left.name.cmp(&right.name).then(left.id.cmp(&right.id)));
        workspaces
    }

    pub fn workspace_roots(&self) -> Vec<PathBuf> {
        let mut roots: Vec<_> = self
            .workspaces
            .values()
            .flat_map(|workspace| workspace.roots.iter().cloned())
            .collect();
        roots.sort();
        roots.dedup();
        roots
    }

    pub fn revision(&self) -> u64 {
        self.revision
    }

    pub fn register_session(
        &mut self,
        session: Session,
    ) -> Result<SessionRegistration, StateError> {
        self.validate_session_registration(&session)?;
        self.sessions.insert(session.id, session.clone());
        self.revision += 1;

        Ok(SessionRegistration {
            session,
            inserted: true,
        })
    }

    pub fn validate_session_registration(&self, session: &Session) -> Result<(), StateError> {
        if !self.workspaces.contains_key(&session.workspace_id) {
            return Err(StateError::WorkspaceNotFound(session.workspace_id));
        }

        if self.sessions.contains_key(&session.id) {
            return Err(StateError::SessionAlreadyExists(session.id));
        }

        Ok(())
    }

    pub fn remove_session(&mut self, session_id: SessionId) -> Result<SessionStop, StateError> {
        if self.sessions.remove(&session_id).is_none() {
            return Err(StateError::SessionNotFound(session_id));
        }
        self.revision += 1;

        Ok(SessionStop {
            session_id,
            removed: true,
        })
    }

    pub fn session(&self, session_id: SessionId) -> Option<&Session> {
        self.sessions.get(&session_id)
    }

    pub fn list_sessions(&self, workspace_id: Option<WorkspaceId>) -> Vec<Session> {
        let mut sessions: Vec<_> = self
            .sessions
            .values()
            .filter(|session| {
                workspace_id
                    .map(|id| session.workspace_id == id)
                    .unwrap_or(true)
            })
            .cloned()
            .collect();

        sessions.sort_by(|left, right| left.cwd.cmp(&right.cwd).then(left.id.cmp(&right.id)));
        sessions
    }

    pub fn session_count(&self) -> usize {
        self.sessions.len()
    }
}

impl SqliteStateStore {
    pub fn open(path: impl Into<PathBuf>) -> Result<Self, StateError> {
        Self::open_with_secrets(path, None)
    }

    pub fn open_with_secrets(
        path: impl Into<PathBuf>,
        secrets: Option<SecretsStore>,
    ) -> Result<Self, StateError> {
        let db_path = path.into();
        if let Some(parent) = db_path.parent() {
            std::fs::create_dir_all(parent).map_err(storage_error)?;
        }

        let connection = Connection::open(&db_path).map_err(storage_error)?;
        initialize_schema(&connection)?;

        Ok(Self { db_path, secrets })
    }

    pub fn db_path(&self) -> &Path {
        &self.db_path
    }

    pub fn secrets(&self) -> Option<SecretsStore> {
        self.secrets.clone()
    }

    pub fn load_snapshot(&self) -> Result<Option<StateSnapshot>, StateError> {
        let connection = Connection::open(&self.db_path).map_err(storage_error)?;
        initialize_schema(&connection)?;

        let mut statement = connection
            .prepare("SELECT snapshot_json FROM canonical_snapshot WHERE slot = 1")
            .map_err(storage_error)?;
        let mut rows = statement.query([]).map_err(storage_error)?;

        let Some(row) = rows.next().map_err(storage_error)? else {
            return Ok(None);
        };

        let mut snapshot_json: String = row.get(0).map_err(storage_error)?;
        if let Some(secrets) = self.secrets.as_ref() {
            snapshot_json = decrypt_if_needed(secrets, snapshot_json)?;
        }
        let snapshot = serde_json::from_str(&snapshot_json)
            .map_err(|error| StateError::Storage(error.to_string()))?;
        Ok(Some(snapshot))
    }

    pub fn save_snapshot(&self, snapshot: &StateSnapshot) -> Result<(), StateError> {
        let connection = Connection::open(&self.db_path).map_err(storage_error)?;
        initialize_schema(&connection)?;

        let mut snapshot_json = serde_json::to_string(snapshot)
            .map_err(|error| StateError::Storage(error.to_string()))?;
        if let Some(secrets) = self.secrets.as_ref() {
            snapshot_json = secrets
                .encrypt_string(&snapshot_json)
                .map_err(secrets_error_to_state_error)?;
        }
        connection
            .execute(
                "INSERT INTO canonical_snapshot (slot, snapshot_json)
                 VALUES (1, ?1)
                 ON CONFLICT(slot) DO UPDATE SET snapshot_json = excluded.snapshot_json",
                params![snapshot_json],
            )
            .map_err(storage_error)?;

        Ok(())
    }

    pub fn load_store(&self) -> Result<Option<CanonicalStore>, StateError> {
        Ok(self.load_snapshot()?.map(CanonicalStore::from_snapshot))
    }

    pub fn save_store(&self, store: &CanonicalStore) -> Result<(), StateError> {
        self.save_snapshot(&store.snapshot())
    }
}

fn unix_ms(time: SystemTime) -> u64 {
    time.duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}

fn time_from_unix_ms(unix_ms: u64) -> SystemTime {
    UNIX_EPOCH + std::time::Duration::from_millis(unix_ms)
}

fn initialize_schema(connection: &Connection) -> Result<(), StateError> {
    connection
        .execute_batch(
            "CREATE TABLE IF NOT EXISTS canonical_snapshot (
                slot INTEGER PRIMARY KEY CHECK (slot = 1),
                snapshot_json TEXT NOT NULL
            );",
        )
        .map_err(storage_error)?;

    Ok(())
}

fn storage_error(error: impl ToString) -> StateError {
    StateError::Storage(error.to_string())
}

fn secrets_error_to_state_error(error: SecretsStoreError) -> StateError {
    StateError::Storage(error.to_string())
}

fn decrypt_if_needed(secrets: &SecretsStore, value: String) -> Result<String, StateError> {
    if SecretsStore::is_encrypted_payload(&value) {
        secrets
            .decrypt_string(&value)
            .map_err(secrets_error_to_state_error)
    } else {
        Ok(value)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ping_updates_health_snapshot() {
        let mut store = CanonicalStore::new_in_memory();
        let initial_health = store.health_snapshot();
        assert_eq!(initial_health.last_ping_at_unix_ms, None);
        assert_eq!(initial_health.workspace_count, 0);

        let ping = store.ping();
        let health = store.health_snapshot();

        assert_eq!(ping.daemon_id, health.daemon_id);
        assert_eq!(health.last_ping_at_unix_ms, Some(ping.at_unix_ms));
        assert!(health.ready);
    }

    #[test]
    fn register_workspace_mutates_canonical_state() {
        let mut store = CanonicalStore::new_in_memory();
        let workspace_id = Uuid::new_v4();

        let registration = store
            .register_workspace(workspace_id, "main", Vec::new())
            .expect("workspace should register");

        assert!(registration.inserted);
        assert_eq!(registration.workspace.id, workspace_id);
        assert_eq!(registration.workspace.name, "main");
        assert_eq!(store.workspace_count(), 1);
        assert_eq!(store.workspace(workspace_id), Some(&registration.workspace));

        let snapshot = store.snapshot();
        assert_eq!(snapshot.workspaces.len(), 1);
        assert!(snapshot.workspaces.contains_key(&workspace_id));
        assert_eq!(snapshot.revision, 1);
    }

    #[test]
    fn register_workspace_rejects_duplicate_ids() {
        let mut store = CanonicalStore::new_in_memory();
        let workspace_id = Uuid::new_v4();

        store
            .register_workspace(workspace_id, "main", Vec::new())
            .expect("first registration should succeed");

        let error = store
            .register_workspace(workspace_id, "other", Vec::new())
            .expect_err("duplicate registration should fail");

        assert_eq!(error, StateError::WorkspaceAlreadyExists(workspace_id));
    }

    #[test]
    fn sqlite_state_store_roundtrips_snapshot() {
        let db_path = test_db_path();
        let persistence = SqliteStateStore::open(&db_path).expect("sqlite state store should open");
        let workspace_id = Uuid::new_v4();
        let session_id = Uuid::new_v4();

        let mut store = CanonicalStore::new_in_memory();
        store
            .register_workspace(workspace_id, "main", Vec::new())
            .expect("workspace should register");
        store
            .register_session(
                Session::new(
                    session_id,
                    workspace_id,
                    core_domain::SessionKind::Local,
                    core_domain::SessionBacking::LocalPty,
                    "/workspace",
                )
                .expect("session should be valid"),
            )
            .expect("session should register");

        persistence
            .save_store(&store)
            .expect("state snapshot should persist");

        let restored = persistence
            .load_store()
            .expect("state snapshot should load")
            .expect("snapshot should exist");

        assert_eq!(restored.workspace_count(), 1);
        assert_eq!(restored.session_count(), 1);
        assert_eq!(restored.revision(), store.revision());

        let _ = std::fs::remove_file(&db_path);
    }

    #[test]
    fn sqlite_state_store_encrypts_snapshot_when_secrets_are_present() {
        let db_path = test_db_path();
        let secrets = SecretsStore::from_passphrase(secrecy::SecretString::from("test-passphrase"))
            .expect("passphrase should initialize");
        let persistence = SqliteStateStore::open_with_secrets(&db_path, Some(secrets))
            .expect("sqlite state store should open");
        let workspace_id = Uuid::new_v4();

        let mut store = CanonicalStore::new_in_memory();
        store
            .register_workspace(workspace_id, "main", Vec::new())
            .expect("workspace should register");

        persistence
            .save_store(&store)
            .expect("encrypted state snapshot should persist");

        let connection = Connection::open(&db_path).expect("sqlite file should open");
        let snapshot_json: String = connection
            .query_row(
                "SELECT snapshot_json FROM canonical_snapshot WHERE slot = 1",
                [],
                |row| row.get(0),
            )
            .expect("snapshot row should exist");
        assert!(SecretsStore::is_encrypted_payload(&snapshot_json));
        assert!(!snapshot_json.contains("\"main\""));

        let restored = persistence
            .load_store()
            .expect("encrypted state snapshot should load")
            .expect("snapshot should exist");
        assert_eq!(restored.workspace_count(), 1);

        let _ = std::fs::remove_file(&db_path);
    }

    fn test_db_path() -> PathBuf {
        PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join(".tmp")
            .join(format!("core-state-{}.sqlite3", Uuid::new_v4()))
    }

    #[test]
    fn register_workspace_rejects_blank_names() {
        let mut store = CanonicalStore::new_in_memory();

        let error = store
            .register_workspace(Uuid::new_v4(), "   ", Vec::new())
            .expect_err("blank workspace names should fail");

        assert_eq!(error, StateError::Domain(DomainError::EmptyWorkspaceName));
    }

    #[test]
    fn register_and_remove_session_follow_workspace_boundary() {
        let mut store = CanonicalStore::new_in_memory();
        let workspace_id = Uuid::new_v4();
        let session_id = Uuid::new_v4();

        store
            .register_workspace(workspace_id, "main", Vec::new())
            .expect("workspace should register");

        let session = Session::new(
            session_id,
            workspace_id,
            core_domain::SessionKind::Local,
            core_domain::SessionBacking::LocalPty,
            "/workspace",
        )
        .expect("session should be valid");

        let registration = store
            .register_session(session.clone())
            .expect("session should register");
        assert!(registration.inserted);
        assert_eq!(registration.session, session);
        assert_eq!(store.session_count(), 1);
        assert_eq!(store.session(session_id), Some(&session));

        let sessions = store.list_sessions(Some(workspace_id));
        assert_eq!(sessions, vec![session.clone()]);

        let stop = store
            .remove_session(session_id)
            .expect("session should stop");
        assert!(stop.removed);
        assert_eq!(stop.session_id, session_id);
        assert_eq!(store.session_count(), 0);
    }

    #[test]
    fn register_session_rejects_unknown_workspace() {
        let mut store = CanonicalStore::new_in_memory();
        let workspace_id = Uuid::new_v4();
        let session = Session::new(
            Uuid::new_v4(),
            workspace_id,
            core_domain::SessionKind::Local,
            core_domain::SessionBacking::LocalPty,
            "/workspace",
        )
        .expect("session should be valid");

        let error = store
            .register_session(session)
            .expect_err("unknown workspace must fail");

        assert_eq!(error, StateError::WorkspaceNotFound(workspace_id));
    }
}

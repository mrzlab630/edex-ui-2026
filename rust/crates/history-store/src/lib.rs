use core_domain::{DomainError, HistoryEntry, HistoryEntryKind, SessionId, WorkspaceId};
use rusqlite::{params, Connection, OptionalExtension};
use secrets_store::{SecretsStore, SecretsStoreError};
use std::path::{Path, PathBuf};
use uuid::Uuid;

#[derive(Debug, Clone)]
pub struct HistoryStore {
    db_path: PathBuf,
    secrets: Option<SecretsStore>,
}

#[derive(Debug, thiserror::Error)]
pub enum HistoryStoreError {
    #[error(transparent)]
    Domain(#[from] DomainError),
    #[error("history storage error: {0}")]
    Storage(String),
}

impl HistoryStore {
    pub fn open(path: impl Into<PathBuf>) -> Result<Self, HistoryStoreError> {
        Self::open_with_secrets(path, None)
    }

    pub fn open_with_secrets(
        path: impl Into<PathBuf>,
        secrets: Option<SecretsStore>,
    ) -> Result<Self, HistoryStoreError> {
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

    pub fn load_all(&self) -> Result<Vec<HistoryEntry>, HistoryStoreError> {
        self.recent(None, None, usize::MAX)
    }

    pub fn append(&self, entry: &HistoryEntry) -> Result<(), HistoryStoreError> {
        let connection = Connection::open(&self.db_path).map_err(storage_error)?;
        initialize_schema(&connection)?;
        let mut content = entry.content.clone();
        if let Some(secrets) = self.secrets.as_ref() {
            content = secrets
                .encrypt_string(&content)
                .map_err(secrets_error_to_history_error)?;
        }

        connection
            .execute(
                "INSERT INTO history_entries
                 (id, workspace_id, session_id, kind, at_unix_ms, content)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
                params![
                    entry.id.to_string(),
                    entry.workspace_id.to_string(),
                    entry.session_id.map(|id| id.to_string()),
                    history_kind_to_db(entry.kind),
                    entry.at_unix_ms as i64,
                    content,
                ],
            )
            .map_err(storage_error)?;

        Ok(())
    }

    pub fn replace_all(&self, entries: &[HistoryEntry]) -> Result<(), HistoryStoreError> {
        let mut connection = Connection::open(&self.db_path).map_err(storage_error)?;
        initialize_schema(&connection)?;
        let transaction = connection.transaction().map_err(storage_error)?;
        transaction
            .execute("DELETE FROM history_entries", [])
            .map_err(storage_error)?;

        for entry in entries {
            let mut content = entry.content.clone();
            if let Some(secrets) = self.secrets.as_ref() {
                content = secrets
                    .encrypt_string(&content)
                    .map_err(secrets_error_to_history_error)?;
            }

            transaction
                .execute(
                    "INSERT INTO history_entries
                     (id, workspace_id, session_id, kind, at_unix_ms, content)
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
                    params![
                        entry.id.to_string(),
                        entry.workspace_id.to_string(),
                        entry.session_id.map(|id| id.to_string()),
                        history_kind_to_db(entry.kind),
                        entry.at_unix_ms as i64,
                        content,
                    ],
                )
                .map_err(storage_error)?;
        }

        transaction.commit().map_err(storage_error)?;
        Ok(())
    }

    pub fn recent(
        &self,
        workspace_id: Option<WorkspaceId>,
        session_id: Option<SessionId>,
        limit: usize,
    ) -> Result<Vec<HistoryEntry>, HistoryStoreError> {
        let connection = Connection::open(&self.db_path).map_err(storage_error)?;
        initialize_schema(&connection)?;

        let mut statement = connection
            .prepare(
                "SELECT id, workspace_id, session_id, kind, at_unix_ms, content
                 FROM history_entries
                 WHERE (?1 IS NULL OR workspace_id = ?1)
                   AND (?2 IS NULL OR session_id = ?2)
                 ORDER BY at_unix_ms DESC, id DESC
                 LIMIT ?3",
            )
            .map_err(storage_error)?;

        let rows = statement
            .query_map(
                params![
                    workspace_id.map(|id| id.to_string()),
                    session_id.map(|id| id.to_string()),
                    limit.min(i64::MAX as usize) as i64,
                ],
                |row| {
                    let entry_id: String = row.get(0)?;
                    let workspace_id: String = row.get(1)?;
                    let session_id: Option<String> = row.get(2)?;
                    let kind: String = row.get(3)?;
                    let at_unix_ms: i64 = row.get(4)?;
                    let mut content: String = row.get(5)?;
                    if let Some(secrets) = self.secrets.as_ref() {
                        content = decrypt_if_needed(secrets, content)?;
                    }

                    let history_entry = HistoryEntry::new(
                        parse_uuid(&entry_id)?,
                        parse_uuid(&workspace_id)?,
                        session_id.as_deref().map(parse_uuid).transpose()?,
                        history_kind_from_db(&kind)?,
                        at_unix_ms.max(0) as u64,
                        content,
                    )
                    .map_err(to_sql_error)?;

                    Ok(history_entry)
                },
            )
            .map_err(storage_error)?;

        let mut entries = Vec::new();
        for row in rows {
            entries.push(row.map_err(storage_error)?);
        }

        Ok(entries)
    }

    pub fn count(&self) -> Result<usize, HistoryStoreError> {
        let connection = Connection::open(&self.db_path).map_err(storage_error)?;
        initialize_schema(&connection)?;

        let count: Option<i64> = connection
            .query_row("SELECT COUNT(*) FROM history_entries", [], |row| row.get(0))
            .optional()
            .map_err(storage_error)?;

        Ok(count.unwrap_or_default().max(0) as usize)
    }
}

fn initialize_schema(connection: &Connection) -> Result<(), HistoryStoreError> {
    connection
        .execute_batch(
            "CREATE TABLE IF NOT EXISTS history_entries (
                id TEXT PRIMARY KEY,
                workspace_id TEXT NOT NULL,
                session_id TEXT NULL,
                kind TEXT NOT NULL,
                at_unix_ms INTEGER NOT NULL,
                content TEXT NOT NULL
            );
            CREATE INDEX IF NOT EXISTS idx_history_workspace_time
                ON history_entries (workspace_id, at_unix_ms DESC);
            CREATE INDEX IF NOT EXISTS idx_history_session_time
                ON history_entries (session_id, at_unix_ms DESC);",
        )
        .map_err(storage_error)?;

    Ok(())
}

fn history_kind_to_db(kind: HistoryEntryKind) -> &'static str {
    match kind {
        HistoryEntryKind::TerminalInput => "terminal_input",
        HistoryEntryKind::TerminalOutput => "terminal_output",
        HistoryEntryKind::ChatUser => "chat_user",
        HistoryEntryKind::ChatAgent => "chat_agent",
        HistoryEntryKind::SystemEvent => "system_event",
    }
}

fn history_kind_from_db(value: &str) -> Result<HistoryEntryKind, rusqlite::Error> {
    match value {
        "terminal_input" => Ok(HistoryEntryKind::TerminalInput),
        "terminal_output" => Ok(HistoryEntryKind::TerminalOutput),
        "chat_user" => Ok(HistoryEntryKind::ChatUser),
        "chat_agent" => Ok(HistoryEntryKind::ChatAgent),
        "system_event" => Ok(HistoryEntryKind::SystemEvent),
        _ => Err(to_sql_error(HistoryStoreError::Storage(format!(
            "unknown history kind `{value}`"
        )))),
    }
}

fn parse_uuid(value: &str) -> Result<Uuid, rusqlite::Error> {
    Uuid::parse_str(value)
        .map_err(|error| to_sql_error(HistoryStoreError::Storage(error.to_string())))
}

fn to_sql_error(error: impl ToString) -> rusqlite::Error {
    rusqlite::Error::FromSqlConversionFailure(
        0,
        rusqlite::types::Type::Text,
        Box::new(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            error.to_string(),
        )),
    )
}

fn storage_error(error: impl ToString) -> HistoryStoreError {
    HistoryStoreError::Storage(error.to_string())
}

fn secrets_error_to_history_error(error: SecretsStoreError) -> HistoryStoreError {
    HistoryStoreError::Storage(error.to_string())
}

fn decrypt_if_needed(secrets: &SecretsStore, value: String) -> Result<String, rusqlite::Error> {
    if SecretsStore::is_encrypted_payload(&value) {
        secrets.decrypt_string(&value).map_err(to_sql_error)
    } else {
        Ok(value)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn history_store_roundtrips_recent_entries() {
        let db_path = test_db_path();
        let store = HistoryStore::open(&db_path).expect("history store should open");
        let workspace_id = Uuid::new_v4();
        let session_id = Uuid::new_v4();

        let first = HistoryEntry::new(
            Uuid::new_v4(),
            workspace_id,
            Some(session_id),
            HistoryEntryKind::TerminalInput,
            1,
            "ls -la",
        )
        .expect("entry should be valid");
        let second = HistoryEntry::new(
            Uuid::new_v4(),
            workspace_id,
            Some(session_id),
            HistoryEntryKind::ChatAgent,
            2,
            "Try `cargo test --workspace`.",
        )
        .expect("entry should be valid");

        store.append(&first).expect("first entry should persist");
        store.append(&second).expect("second entry should persist");

        let recent = store
            .recent(Some(workspace_id), Some(session_id), 10)
            .expect("recent entries should load");
        assert_eq!(recent.len(), 2);
        assert_eq!(recent[0].id, second.id);
        assert_eq!(recent[1].id, first.id);
        assert_eq!(store.count().expect("count should load"), 2);

        let _ = std::fs::remove_file(db_path);
    }

    #[test]
    fn history_store_replace_all_resets_transcript() {
        let db_path = test_db_path();
        let store = HistoryStore::open(&db_path).expect("history store should open");
        let workspace_id = Uuid::new_v4();
        let replacement = HistoryEntry::new(
            Uuid::new_v4(),
            workspace_id,
            None,
            HistoryEntryKind::SystemEvent,
            9,
            "replacement event",
        )
        .expect("entry should be valid");

        store
            .replace_all(std::slice::from_ref(&replacement))
            .expect("replace_all should succeed");

        let entries = store.load_all().expect("entries should load");
        assert_eq!(entries, vec![replacement]);

        let _ = std::fs::remove_file(db_path);
    }

    #[test]
    fn history_store_encrypts_entry_content_when_secrets_are_present() {
        let db_path = test_db_path();
        let secrets = SecretsStore::from_passphrase(secrecy::SecretString::from("test-passphrase"))
            .expect("passphrase should initialize");
        let store = HistoryStore::open_with_secrets(&db_path, Some(secrets))
            .expect("history store should open");
        let workspace_id = Uuid::new_v4();

        let entry = HistoryEntry::new(
            Uuid::new_v4(),
            workspace_id,
            None,
            HistoryEntryKind::ChatAgent,
            1,
            "sensitive terminal transcript",
        )
        .expect("entry should be valid");

        store.append(&entry).expect("entry should persist");
        let connection = Connection::open(&db_path).expect("sqlite file should open");
        let stored_content: String = connection
            .query_row("SELECT content FROM history_entries LIMIT 1", [], |row| {
                row.get(0)
            })
            .expect("history row should exist");
        assert!(SecretsStore::is_encrypted_payload(&stored_content));
        assert!(!stored_content.contains("sensitive terminal transcript"));

        let restored = store
            .recent(Some(workspace_id), None, 10)
            .expect("recent entries should decrypt");
        assert_eq!(restored[0].content, "sensitive terminal transcript");

        let _ = std::fs::remove_file(db_path);
    }

    fn test_db_path() -> PathBuf {
        PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join(".tmp")
            .join(format!("history-store-{}.sqlite3", Uuid::new_v4()))
    }
}

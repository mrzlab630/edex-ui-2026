use core_domain::HistoryEntry;
use core_state::StateSnapshot;
use runtime_api::SshHostProfile;
use secrets_store::{SecretsStore, SecretsStoreError};
use serde::{Deserialize, Serialize};
use std::fs::File;
use std::io::{BufReader, BufWriter, Write};
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

const RECOVERY_SCHEMA_VERSION: u32 = 1;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct RecoveryBundle {
    pub schema_version: u32,
    pub exported_at_unix_ms: u64,
    pub state_snapshot: StateSnapshot,
    pub history_entries: Vec<HistoryEntry>,
    pub ssh_hosts: Vec<SshHostProfile>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct RecoveryBundleSummary {
    pub bundle_path: PathBuf,
    pub workspace_count: usize,
    pub session_count: usize,
    pub history_count: usize,
    pub ssh_host_count: usize,
}

#[derive(Debug, thiserror::Error)]
pub enum RecoveryError {
    #[error("recovery requires configured encryption secrets")]
    MissingSecrets,
    #[error("recovery bundle error: {0}")]
    Storage(String),
    #[error(transparent)]
    Secrets(#[from] SecretsStoreError),
}

impl RecoveryBundle {
    pub fn new(
        state_snapshot: StateSnapshot,
        history_entries: Vec<HistoryEntry>,
        ssh_hosts: Vec<SshHostProfile>,
    ) -> Self {
        Self {
            schema_version: RECOVERY_SCHEMA_VERSION,
            exported_at_unix_ms: unix_ms(SystemTime::now()),
            state_snapshot,
            history_entries,
            ssh_hosts,
        }
    }

    pub fn summary_for_path(&self, bundle_path: impl Into<PathBuf>) -> RecoveryBundleSummary {
        RecoveryBundleSummary {
            bundle_path: bundle_path.into(),
            workspace_count: self.state_snapshot.workspaces.len(),
            session_count: self.state_snapshot.sessions.len(),
            history_count: self.history_entries.len(),
            ssh_host_count: self.ssh_hosts.len(),
        }
    }
}

pub fn export_bundle(
    bundle_path: impl AsRef<Path>,
    bundle: &RecoveryBundle,
    secrets: &SecretsStore,
) -> Result<RecoveryBundleSummary, RecoveryError> {
    let bundle_path = bundle_path.as_ref();
    if let Some(parent) = bundle_path.parent() {
        std::fs::create_dir_all(parent).map_err(storage_error)?;
    }

    let file = File::create(bundle_path).map_err(storage_error)?;
    let mut writer = BufWriter::new(file);
    secrets
        .encrypt_json_to_writer(&mut writer, bundle)
        .map_err(RecoveryError::from)?;

    Ok(bundle.summary_for_path(bundle_path))
}

pub fn import_bundle(
    bundle_path: impl AsRef<Path>,
    secrets: &SecretsStore,
) -> Result<RecoveryBundle, RecoveryError> {
    let file = File::open(bundle_path).map_err(storage_error)?;
    let reader = BufReader::new(file);
    secrets.decrypt_json_from_reader(reader).map_err(RecoveryError::from)
}

pub fn rekey_bundle(
    input_path: impl AsRef<Path>,
    output_path: impl AsRef<Path>,
    current_secrets: &SecretsStore,
    new_secrets: &SecretsStore,
) -> Result<RecoveryBundleSummary, RecoveryError> {
    let input_path = input_path.as_ref();
    let output_path = output_path.as_ref();
    if let Some(parent) = output_path.parent() {
        std::fs::create_dir_all(parent).map_err(storage_error)?;
    }

    let input = File::open(input_path).map_err(storage_error)?;
    let output = File::create(output_path).map_err(storage_error)?;
    let mut output_writer = BufWriter::new(output);
    current_secrets
        .reencrypt_reader_to_writer(BufReader::new(input), new_secrets, &mut output_writer)
        .map_err(RecoveryError::from)?;
    output_writer.flush().map_err(storage_error)?;
    let bundle = import_bundle(output_path, new_secrets)?;
    Ok(bundle.summary_for_path(output_path))
}

fn storage_error(error: impl ToString) -> RecoveryError {
    RecoveryError::Storage(error.to_string())
}

fn unix_ms(time: SystemTime) -> u64 {
    time.duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_millis().min(u128::from(u64::MAX)) as u64)
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;
    use core_domain::HistoryEntryKind;
    use runtime_api::SshTcpForward;
    use secrecy::SecretString;
    use uuid::Uuid;

    #[test]
    fn recovery_bundle_export_import_roundtrips() {
        let bundle_path = test_bundle_path("recovery");
        let secrets = SecretsStore::from_passphrase(SecretString::from("test-passphrase"))
            .expect("secrets should initialize");
        let bundle = test_bundle();

        let summary = export_bundle(&bundle_path, &bundle, &secrets).expect("bundle should export");
        assert_eq!(summary.workspace_count, 1);
        assert_eq!(summary.history_count, 1);

        let restored = import_bundle(&bundle_path, &secrets).expect("bundle should import");
        assert_eq!(restored, bundle);

        let raw = std::fs::read(&bundle_path).expect("bundle file should exist");
        assert!(SecretsStore::is_encrypted_stream_payload(&raw));

        let _ = std::fs::remove_file(bundle_path);
    }

    #[test]
    fn recovery_bundle_rekey_roundtrips() {
        let input_path = test_bundle_path("recovery-input");
        let output_path = test_bundle_path("recovery-output");
        let current = SecretsStore::from_passphrase(SecretString::from("current-passphrase"))
            .expect("current secrets should initialize");
        let next = SecretsStore::from_passphrase(SecretString::from("next-passphrase"))
            .expect("next secrets should initialize");
        let bundle = test_bundle();

        export_bundle(&input_path, &bundle, &current).expect("bundle should export");
        let summary =
            rekey_bundle(&input_path, &output_path, &current, &next).expect("bundle should rekey");
        assert_eq!(summary.ssh_host_count, 1);

        let restored = import_bundle(&output_path, &next).expect("rekeyed bundle should import");
        assert_eq!(restored, bundle);

        let _ = std::fs::remove_file(input_path);
        let _ = std::fs::remove_file(output_path);
    }

    fn test_bundle() -> RecoveryBundle {
        let workspace_id = Uuid::new_v4();
        let session_id = Uuid::new_v4();
        let history_entry = HistoryEntry::new(
            Uuid::new_v4(),
            workspace_id,
            Some(session_id),
            HistoryEntryKind::ChatAgent,
            42,
            "cargo test failed with sqlite lock",
        )
        .expect("history entry should be valid");
        let state_snapshot = StateSnapshot {
            daemon_id: Uuid::new_v4(),
            started_at_unix_ms: 10,
            last_ping_at_unix_ms: Some(11),
            schema_version: 1,
            revision: 2,
            workspaces: [(
                workspace_id,
                core_domain::Workspace::new(workspace_id, "alpha")
                    .expect("workspace should be valid"),
            )]
            .into_iter()
            .collect(),
            sessions: [(
                session_id,
                core_domain::Session::new(
                    session_id,
                    workspace_id,
                    core_domain::SessionKind::Local,
                    core_domain::SessionBacking::LocalPty,
                    "/workspace",
                )
                .expect("session should be valid"),
            )]
            .into_iter()
            .collect(),
        };
        let ssh_hosts = vec![SshHostProfile {
            alias: "alpha".into(),
            hostname: "alpha.example".into(),
            user: Some("dev".into()),
            port: Some(2222),
            identity_file: Some("/keys/dev".into()),
            proxy_jump: None,
            local_forwards: vec![SshTcpForward {
                bind_address: "127.0.0.1".into(),
                bind_port: 15432,
                target_host: "db.internal".into(),
                target_port: 5432,
            }],
            remote_forwards: Vec::new(),
            dynamic_forwards: Vec::new(),
        }];

        RecoveryBundle::new(state_snapshot, vec![history_entry], ssh_hosts)
    }

    fn test_bundle_path(prefix: &str) -> PathBuf {
        let stamp = unix_ms(SystemTime::now());
        std::env::temp_dir().join(format!("{prefix}-{stamp}.edex-recovery"))
    }
}

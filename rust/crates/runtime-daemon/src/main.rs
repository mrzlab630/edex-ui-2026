use anyhow::Result;
use core_state::SqliteStateStore;
use runtime_api::API_VERSION;
use runtime_daemon::{bind_listener, run_daemon, DaemonState};
use secrets_store::{SecretSource, SecretsStore};
use std::path::PathBuf;
use std::sync::Arc;
use tracing::info;
use tracing_subscriber::EnvFilter;

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info")),
        )
        .with_target(false)
        .compact()
        .init();

    let socket_path = default_socket_path();
    info!(api_version = API_VERSION, socket_path = %socket_path.display(), "runtime-daemon bootstrap ready");
    info!("phase-1 workspace scaffold is ready");
    let resolved_secrets = SecretsStore::resolve()?;
    if let Some(resolved) = resolved_secrets.as_ref() {
        info!(secret_source = ?resolved.source, "master secret resolved");
        if matches!(resolved.source, SecretSource::SecretService) {
            info!("using Secret Service as Linux-native master key source");
        }
    }
    let secrets = resolved_secrets.map(|resolved| resolved.store);
    let persistence = state_db_path()
        .map(|path| SqliteStateStore::open_with_secrets(path, secrets.clone()))
        .transpose()?;

    if bootstrap_only() {
        info!("bootstrap-only mode enabled; exiting before socket bind");
        return Ok(());
    }

    let listener = bind_listener(&socket_path).await?;
    let state = Arc::new(match persistence {
        Some(persistence) => DaemonState::try_new_with_persistence_and_secrets(
            socket_path.clone(),
            persistence,
            secrets.clone(),
        )?,
        None => DaemonState::new_with_secrets(socket_path.clone(), secrets.clone()),
    });

    run_daemon(listener, state).await?;

    Ok(())
}

fn default_socket_path() -> PathBuf {
    if let Ok(path) = std::env::var("EDEX_CORE_SOCKET") {
        return PathBuf::from(path);
    }

    if let Ok(runtime_dir) = std::env::var("XDG_RUNTIME_DIR") {
        return PathBuf::from(runtime_dir).join("edex-ui-2026-rust-core.sock");
    }

    std::env::temp_dir().join("edex-ui-2026-rust-core.sock")
}

fn bootstrap_only() -> bool {
    matches!(
        std::env::var("EDEX_CORE_BOOTSTRAP_ONLY").as_deref(),
        Ok("1") | Ok("true") | Ok("yes")
    )
}

fn state_db_path() -> Option<PathBuf> {
    std::env::var("EDEX_CORE_STATE_DB").ok().map(PathBuf::from)
}

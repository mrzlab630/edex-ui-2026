use anyhow::{anyhow, Context, Result};
use claw_bridge::ClawProvider;
use core_domain::{AgentPermissionMode, HistoryEntryKind, SessionBacking, SessionKind};
use core_state::SqliteStateStore;
use runtime_api::{
    decode_json_frame, encode_json_frame, Command, Query, RequestEnvelope, ResponseEnvelope,
    ResponsePayload,
};
use runtime_daemon::{bind_listener, run_daemon, DaemonState};
use secrecy::SecretString;
use secrets_store::SecretsStore;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use tmux_bridge::TmuxRuntime;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::UnixStream;
use uuid::Uuid;

#[tokio::main]
async fn main() -> Result<()> {
    let root = acceptance_root();
    std::fs::create_dir_all(&root)?;

    let fake_claw_path = root.join("fake-claw");
    let workspace_root = root.join("workspace");
    let primary_socket = root.join("runtime.sock");
    let primary_state_db = root.join("state.sqlite3");
    let bundle_path = root.join("recovery.bundle");
    let restore_socket = root.join("restored.sock");
    let restore_state_db = root.join("restored.sqlite3");
    let tmux_socket = root.join("tmux.sock");

    write_fake_claw(&fake_claw_path)?;
    write_workspace_fixture(&workspace_root)?;

    let secrets = SecretsStore::from_passphrase(SecretString::from("host-acceptance-passphrase"))?;
    let primary_tmux = TmuxRuntime::isolated(&tmux_socket);
    let primary_claw = ClawProvider::new(&fake_claw_path)?;

    let primary_state = spawn_daemon(
        &primary_socket,
        &primary_state_db,
        primary_tmux.clone(),
        Some(secrets.clone()),
        Some(primary_claw),
    )
    .await?;

    let workspace_id = Uuid::new_v4();
    let local_session_id = Uuid::new_v4();
    let tmux_session_id = Uuid::new_v4();

    expect_health(
        send(
            &primary_socket,
            RequestEnvelope::query("req-health", Query::Health),
        )
        .await?,
    )?;

    expect_workspace_registered(
        send(
            &primary_socket,
            RequestEnvelope::command(
                "req-workspace",
                Command::RegisterWorkspace {
                    workspace_id,
                    name: "acceptance".into(),
                    roots: vec![workspace_root.display().to_string()],
                },
            ),
        )
        .await?,
        1,
    )?;

    expect_session_registered(
        send(
            &primary_socket,
            RequestEnvelope::command(
                "req-session",
                Command::RegisterSession {
                    session_id: local_session_id,
                    workspace_id,
                    kind: SessionKind::Local,
                    backing: SessionBacking::LocalPty,
                    cwd: workspace_root.display().to_string(),
                },
            ),
        )
        .await?,
        local_session_id,
        1,
    )?;

    expect_file_index_refreshed(
        send(
            &primary_socket,
            RequestEnvelope::command(
                "req-file-refresh",
                Command::RefreshFileIndex {
                    workspace_id,
                    root_path: workspace_root.display().to_string(),
                },
            ),
        )
        .await?,
    )?;
    expect_file_search(
        send(
            &primary_socket,
            RequestEnvelope::query(
                "req-file-search",
                Query::FileSearch {
                    workspace_id,
                    root_path: workspace_root.display().to_string(),
                    text: "cargo readme".into(),
                    limit: 5,
                },
            ),
        )
        .await?,
    )?;

    expect_history_appended(
        send(
            &primary_socket,
            RequestEnvelope::command(
                "req-history",
                Command::AppendHistoryEntry {
                    entry_id: Uuid::new_v4(),
                    workspace_id,
                    session_id: Some(local_session_id),
                    kind: HistoryEntryKind::TerminalOutput,
                    at_unix_ms: 1,
                    content: "cargo test failed with sqlite lock".into(),
                },
            ),
        )
        .await?,
        1,
    )?;
    expect_context_results(
        send(
            &primary_socket,
            RequestEnvelope::query(
                "req-context",
                Query::ContextSearch {
                    workspace_id: Some(workspace_id),
                    session_id: Some(local_session_id),
                    text: "sqlite lock".into(),
                    limit: 5,
                },
            ),
        )
        .await?,
        1,
    )?;

    expect_ssh_imported(
        send(
            &primary_socket,
            RequestEnvelope::command(
                "req-ssh-import",
                Command::ImportSshConfig {
                    config_text: "Host alpha\n  HostName alpha.example\n  User dev\n".into(),
                },
            ),
        )
        .await?,
        1,
    )?;
    expect_ssh_hosts(
        send(
            &primary_socket,
            RequestEnvelope::query("req-ssh-hosts", Query::SshHosts),
        )
        .await?,
        1,
    )?;

    expect_agent_provider_status(
        send(
            &primary_socket,
            RequestEnvelope::query("req-agent-status", Query::AgentProviderStatus),
        )
        .await?,
    )?;
    expect_agent_task_completed(
        send(
            &primary_socket,
            RequestEnvelope::command(
                "req-agent-run",
                Command::RunAgentTask {
                    task_id: Uuid::new_v4(),
                    workspace_id,
                    cwd: Some(workspace_root.display().to_string()),
                    prompt: "Summarize the current workspace state".into(),
                    model: Some("claude-opus-4-6".into()),
                    context_query: Some("cargo test".into()),
                    context_limit: 5,
                    permission_mode: AgentPermissionMode::WorkspaceWrite,
                    allowed_tools: vec!["read".into(), "glob".into()],
                },
            ),
        )
        .await?,
        workspace_id,
    )?;

    expect_session_registered(
        send(
            &primary_socket,
            RequestEnvelope::command(
                "req-tmux-register",
                Command::RegisterLocalTmuxSession {
                    session_id: tmux_session_id,
                    workspace_id,
                    cwd: workspace_root.display().to_string(),
                },
            ),
        )
        .await?,
        tmux_session_id,
        2,
    )?;
    expect_session_removed(
        send(
            &primary_socket,
            RequestEnvelope::command(
                "req-tmux-remove",
                Command::RemoveSession {
                    session_id: tmux_session_id,
                },
            ),
        )
        .await?,
        tmux_session_id,
        1,
    )?;
    let remaining_tmux = primary_tmux.list_sessions()?;
    if !remaining_tmux.is_empty() {
        return Err(anyhow!(
            "expected isolated tmux runtime to be empty after removal, found {} sessions",
            remaining_tmux.len()
        ));
    }

    expect_bundle_exported(
        send(
            &primary_socket,
            RequestEnvelope::command(
                "req-export",
                Command::ExportRecoveryBundle {
                    bundle_path: bundle_path.display().to_string(),
                },
            ),
        )
        .await?,
    )?;

    primary_state.abort();

    let restored_state = spawn_daemon(
        &restore_socket,
        &restore_state_db,
        TmuxRuntime::isolated(root.join("restored-tmux.sock")),
        Some(secrets),
        Some(ClawProvider::new(&fake_claw_path)?),
    )
    .await?;

    expect_bundle_imported(
        send(
            &restore_socket,
            RequestEnvelope::command(
                "req-import",
                Command::ImportRecoveryBundle {
                    bundle_path: bundle_path.display().to_string(),
                },
            ),
        )
        .await?,
    )?;
    expect_sessions(
        send(
            &restore_socket,
            RequestEnvelope::query(
                "req-restored-sessions",
                Query::Sessions {
                    workspace_id: Some(workspace_id),
                },
            ),
        )
        .await?,
        1,
    )?;
    expect_history_entries(
        send(
            &restore_socket,
            RequestEnvelope::query(
                "req-restored-history",
                Query::RecentHistory {
                    workspace_id: Some(workspace_id),
                    session_id: None,
                    limit: 10,
                },
            ),
        )
        .await?,
        3,
    )?;

    restored_state.abort();
    primary_tmux.kill_server()?;
    println!("HOST ACCEPTANCE OK");
    Ok(())
}

async fn spawn_daemon(
    socket_path: &Path,
    state_db_path: &Path,
    tmux: TmuxRuntime,
    secrets: Option<SecretsStore>,
    claw: Option<ClawProvider>,
) -> Result<tokio::task::JoinHandle<Result<()>>> {
    let persistence = SqliteStateStore::open_with_secrets(state_db_path, secrets.clone())?;
    let state = DaemonState::try_new_with_tmux_persistence_secrets_and_claw(
        socket_path.to_path_buf(),
        tmux,
        Some(persistence),
        secrets,
        claw,
    )?;
    let listener = bind_listener(socket_path).await?;
    let join = tokio::spawn(async move { run_daemon(listener, Arc::new(state)).await });

    wait_for_socket(socket_path).await?;
    Ok(join)
}

async fn wait_for_socket(socket_path: &Path) -> Result<()> {
    for _ in 0..50 {
        if socket_path.exists() {
            return Ok(());
        }
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    }

    Err(anyhow!(
        "socket did not appear at {}",
        socket_path.display()
    ))
}

async fn send(socket_path: &Path, request: RequestEnvelope) -> Result<ResponseEnvelope> {
    let mut stream = UnixStream::connect(socket_path)
        .await
        .with_context(|| format!("connect uds {}", socket_path.display()))?;
    let frame = encode_json_frame(&request)?;
    stream
        .write_all(&frame)
        .await
        .context("write request frame")?;
    stream.shutdown().await.context("shutdown request stream")?;

    let mut lines = BufReader::new(stream).lines();
    let line = lines
        .next_line()
        .await
        .context("read response line")?
        .ok_or_else(|| anyhow!("missing response line"))?;

    let response: ResponseEnvelope =
        decode_json_frame(&line)?.ok_or_else(|| anyhow!("response frame was blank"))?;
    if !response.ok {
        return Err(anyhow!("daemon returned error: {:?}", response.payload));
    }

    Ok(response)
}

fn expect_health(response: ResponseEnvelope) -> Result<()> {
    match response.payload {
        ResponsePayload::Health(snapshot)
            if snapshot.status == runtime_api::RuntimeStatus::Ready =>
        {
            Ok(())
        }
        other => Err(anyhow!("unexpected health payload: {other:?}")),
    }
}

fn expect_workspace_registered(response: ResponseEnvelope, workspace_count: usize) -> Result<()> {
    match response.payload {
        ResponsePayload::WorkspaceRegistered {
            workspace_count: count,
        } if count == workspace_count => Ok(()),
        other => Err(anyhow!("unexpected workspace payload: {other:?}")),
    }
}

fn expect_session_registered(
    response: ResponseEnvelope,
    session_id: Uuid,
    session_count: usize,
) -> Result<()> {
    match response.payload {
        ResponsePayload::SessionRegistered {
            session,
            session_count: count,
        } if session.id == session_id && count == session_count => Ok(()),
        other => Err(anyhow!("unexpected session registered payload: {other:?}")),
    }
}

fn expect_session_removed(
    response: ResponseEnvelope,
    session_id: Uuid,
    session_count: usize,
) -> Result<()> {
    match response.payload {
        ResponsePayload::SessionRemoved {
            session_id: removed,
            session_count: count,
        } if removed == session_id && count == session_count => Ok(()),
        other => Err(anyhow!("unexpected session removed payload: {other:?}")),
    }
}

fn expect_file_index_refreshed(response: ResponseEnvelope) -> Result<()> {
    match response.payload {
        ResponsePayload::FileIndexRefreshed { indexed_count, .. } if indexed_count >= 2 => Ok(()),
        other => Err(anyhow!("unexpected file refresh payload: {other:?}")),
    }
}

fn expect_file_search(response: ResponseEnvelope) -> Result<()> {
    match response.payload {
        ResponsePayload::FileSearchResults { results }
            if !results.is_empty() && results[0].entry.path.ends_with("README.md") =>
        {
            Ok(())
        }
        other => Err(anyhow!("unexpected file search payload: {other:?}")),
    }
}

fn expect_history_appended(response: ResponseEnvelope, history_count: usize) -> Result<()> {
    match response.payload {
        ResponsePayload::HistoryAppended {
            history_count: count,
        } if count == history_count => Ok(()),
        other => Err(anyhow!("unexpected history append payload: {other:?}")),
    }
}

fn expect_context_results(response: ResponseEnvelope, expected_min: usize) -> Result<()> {
    match response.payload {
        ResponsePayload::ContextResults { results } if results.len() >= expected_min => Ok(()),
        other => Err(anyhow!("unexpected context payload: {other:?}")),
    }
}

fn expect_ssh_imported(response: ResponseEnvelope, host_count: usize) -> Result<()> {
    match response.payload {
        ResponsePayload::SshConfigImported { host_count: count } if count == host_count => Ok(()),
        other => Err(anyhow!("unexpected ssh import payload: {other:?}")),
    }
}

fn expect_ssh_hosts(response: ResponseEnvelope, host_count: usize) -> Result<()> {
    match response.payload {
        ResponsePayload::SshHosts { hosts } if hosts.len() == host_count => Ok(()),
        other => Err(anyhow!("unexpected ssh hosts payload: {other:?}")),
    }
}

fn expect_agent_provider_status(response: ResponseEnvelope) -> Result<()> {
    match response.payload {
        ResponsePayload::AgentProviderStatus { status }
            if status.available
                && status.provider == "claw"
                && status.doctor_report.as_deref() == Some("Doctor OK") =>
        {
            Ok(())
        }
        other => Err(anyhow!("unexpected agent provider payload: {other:?}")),
    }
}

fn expect_agent_task_completed(response: ResponseEnvelope, workspace_id: Uuid) -> Result<()> {
    match response.payload {
        ResponsePayload::AgentTaskCompleted {
            task,
            output,
            context_result_count,
            history_count,
        } if task.workspace_id == workspace_id
            && task.permission_mode == AgentPermissionMode::WorkspaceWrite
            && output.contains("FAKE CLAW OUTPUT")
            && context_result_count >= 1
            && history_count >= 3 =>
        {
            Ok(())
        }
        other => Err(anyhow!("unexpected agent task payload: {other:?}")),
    }
}

fn expect_bundle_exported(response: ResponseEnvelope) -> Result<()> {
    match response.payload {
        ResponsePayload::RecoveryBundleExported {
            workspace_count,
            session_count,
            history_count,
            ssh_host_count,
            ..
        } if workspace_count == 1
            && session_count == 1
            && history_count >= 3
            && ssh_host_count == 1 =>
        {
            Ok(())
        }
        other => Err(anyhow!("unexpected recovery export payload: {other:?}")),
    }
}

fn expect_bundle_imported(response: ResponseEnvelope) -> Result<()> {
    match response.payload {
        ResponsePayload::RecoveryBundleImported {
            workspace_count,
            session_count,
            history_count,
            ssh_host_count,
            ..
        } if workspace_count == 1
            && session_count == 1
            && history_count >= 3
            && ssh_host_count == 1 =>
        {
            Ok(())
        }
        other => Err(anyhow!("unexpected recovery import payload: {other:?}")),
    }
}

fn expect_sessions(response: ResponseEnvelope, expected_count: usize) -> Result<()> {
    match response.payload {
        ResponsePayload::Sessions { sessions } if sessions.len() == expected_count => Ok(()),
        other => Err(anyhow!("unexpected sessions payload: {other:?}")),
    }
}

fn expect_history_entries(response: ResponseEnvelope, expected_min: usize) -> Result<()> {
    match response.payload {
        ResponsePayload::HistoryEntries { entries } if entries.len() >= expected_min => Ok(()),
        other => Err(anyhow!("unexpected history payload: {other:?}")),
    }
}

fn acceptance_root() -> PathBuf {
    std::env::temp_dir().join(format!("edex-ui-2026-host-acceptance-{}", Uuid::new_v4()))
}

fn write_workspace_fixture(root: &Path) -> Result<()> {
    std::fs::create_dir_all(root.join("src"))?;
    std::fs::write(root.join("src/lib.rs"), "pub fn cargo_test() {}\n")?;
    std::fs::write(root.join("README.md"), "cargo test guidance\n")?;
    Ok(())
}

fn write_fake_claw(path: &Path) -> Result<()> {
    std::fs::write(
        path,
        r#"#!/bin/sh
case "$*" in
  *version*) printf 'Claw Code\nVersion acceptance\n' ;;
  *doctor*) printf 'Doctor OK\n' ;;
  *status*) printf 'Status OK\n' ;;
  *prompt*) printf 'FAKE CLAW OUTPUT\n' ;;
  *) printf 'unexpected args: %s\n' "$*" >&2; exit 1 ;;
esac
"#,
    )?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mut permissions = std::fs::metadata(path)?.permissions();
        permissions.set_mode(0o755);
        std::fs::set_permissions(path, permissions)?;
    }
    Ok(())
}

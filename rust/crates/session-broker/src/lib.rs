use core_domain::{DomainError, Session, SessionBacking, SessionId, SessionKind, WorkspaceId};
use core_state::{CanonicalStore, SessionRegistration, SessionStop, StateError};
use std::path::PathBuf;
use tmux_bridge::TmuxSessionName;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RegisterSessionRequest {
    pub session_id: SessionId,
    pub workspace_id: WorkspaceId,
    pub kind: SessionKind,
    pub backing: SessionBacking,
    pub cwd: PathBuf,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct SessionFilter {
    pub workspace_id: Option<WorkspaceId>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlannedLocalTmuxSession {
    pub session: Session,
    pub tmux_session_name: TmuxSessionName,
}

#[derive(Debug, thiserror::Error)]
pub enum SessionBrokerError {
    #[error(transparent)]
    Domain(#[from] DomainError),
    #[error(transparent)]
    State(#[from] StateError),
    #[error(transparent)]
    Tmux(#[from] tmux_bridge::TmuxError),
    #[error("blocking task failed: {0}")]
    BlockingTask(String),
}

pub fn register_session(
    store: &mut CanonicalStore,
    request: RegisterSessionRequest,
) -> Result<SessionRegistration, SessionBrokerError> {
    let session = build_session(request)?;
    Ok(store.register_session(session)?)
}

pub fn remove_session(
    store: &mut CanonicalStore,
    session_id: SessionId,
) -> Result<SessionStop, SessionBrokerError> {
    Ok(store.remove_session(session_id)?)
}

pub fn list_sessions(store: &CanonicalStore, filter: SessionFilter) -> Vec<Session> {
    store.list_sessions(filter.workspace_id)
}

pub fn preflight_session_registration(
    store: &CanonicalStore,
    session: &Session,
) -> Result<(), SessionBrokerError> {
    Ok(store.validate_session_registration(session)?)
}

pub fn commit_session_registration(
    store: &mut CanonicalStore,
    session: Session,
) -> Result<SessionRegistration, SessionBrokerError> {
    Ok(store.register_session(session)?)
}

pub fn plan_local_tmux_session(
    request: RegisterSessionRequest,
) -> Result<PlannedLocalTmuxSession, SessionBrokerError> {
    let session = build_session(RegisterSessionRequest {
        kind: SessionKind::Tmux,
        backing: SessionBacking::TmuxSession,
        ..request
    })?;
    let session_name = TmuxSessionName::new(format!("edex-{}", request.session_id.simple()))?;

    Ok(PlannedLocalTmuxSession {
        session,
        tmux_session_name: session_name,
    })
}

fn build_session(request: RegisterSessionRequest) -> Result<Session, SessionBrokerError> {
    Ok(Session::new(
        request.session_id,
        request.workspace_id,
        request.kind,
        request.backing,
        request.cwd,
    )?)
}

#[cfg(test)]
mod tests {
    use super::*;
    use core_state::CanonicalStore;
    use uuid::Uuid;

    #[test]
    fn broker_starts_and_lists_sessions() {
        let mut store = CanonicalStore::new_in_memory();
        let workspace_id = Uuid::new_v4();
        store
            .register_workspace(workspace_id, "main", Vec::new())
            .expect("workspace should register");

        let session_id = Uuid::new_v4();
        let registration = register_session(
            &mut store,
            RegisterSessionRequest {
                session_id,
                workspace_id,
                kind: SessionKind::Local,
                backing: SessionBacking::LocalPty,
                cwd: PathBuf::from("/workspace"),
            },
        )
        .expect("session should start");

        assert_eq!(registration.session.id, session_id);

        let sessions = list_sessions(
            &store,
            SessionFilter {
                workspace_id: Some(workspace_id),
            },
        );
        assert_eq!(sessions.len(), 1);
        assert_eq!(sessions[0].id, session_id);
    }

    #[test]
    fn broker_stops_sessions() {
        let mut store = CanonicalStore::new_in_memory();
        let workspace_id = Uuid::new_v4();
        let session_id = Uuid::new_v4();

        store
            .register_workspace(workspace_id, "main", Vec::new())
            .expect("workspace should register");
        register_session(
            &mut store,
            RegisterSessionRequest {
                session_id,
                workspace_id,
                kind: SessionKind::Local,
                backing: SessionBacking::LocalPty,
                cwd: PathBuf::from("/workspace"),
            },
        )
        .expect("session should start");

        let stop = remove_session(&mut store, session_id).expect("session should stop");
        assert!(stop.removed);
        assert!(list_sessions(&store, SessionFilter::default()).is_empty());
    }

    #[test]
    fn broker_plans_local_tmux_session() {
        let mut store = CanonicalStore::new_in_memory();
        let workspace_id = Uuid::new_v4();
        let session_id = Uuid::new_v4();
        let cwd = PathBuf::from(env!("CARGO_MANIFEST_DIR"));

        store
            .register_workspace(workspace_id, "main", Vec::new())
            .expect("workspace should register");

        let planned = plan_local_tmux_session(RegisterSessionRequest {
            session_id,
            workspace_id,
            kind: SessionKind::Tmux,
            backing: SessionBacking::TmuxSession,
            cwd: cwd.clone(),
        })
        .expect("tmux-backed session should plan");

        preflight_session_registration(&store, &planned.session)
            .expect("planned session should pass store validation");
        let registration = commit_session_registration(&mut store, planned.session)
            .expect("planned session should commit");

        assert_eq!(registration.session.id, session_id);
        assert_eq!(registration.session.kind, SessionKind::Tmux);
        assert_eq!(registration.session.backing, SessionBacking::TmuxSession);
        assert_eq!(registration.session.cwd, cwd);
    }
}

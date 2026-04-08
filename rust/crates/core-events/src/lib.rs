use serde::{Deserialize, Serialize};
use uuid::Uuid;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub enum CoreEvent {
    WorkspaceRegistered { workspace_id: Uuid },
    SessionStarted { session_id: Uuid },
    SessionStopped { session_id: Uuid },
    AgentTaskQueued { task_id: Uuid },
}

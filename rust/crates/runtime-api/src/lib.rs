mod framing;
mod protocol;

pub use context_engine::ContextResult;
pub use core_domain::{AgentPermissionMode, HistoryEntry, Session};
pub use file_index::{FileEntry, FilePreview, FileSearchResult};
pub use file_index::FileKind;
pub use framing::{
    decode_json_frame, encode_json_frame, FrameAssumptions, FrameEncoding, FrameError,
    FRAME_CONTENT_TYPE, FRAME_DELIMITER, FRAME_DELIMITER_STR, LOCAL_TRANSPORT, MAX_FRAME_BYTES,
};
pub use protocol::{
    AgentProviderStatus, ApiError, Command, DaemonInfo, ErrorCode, EventEnvelope, HealthSnapshot,
    Query, RequestEnvelope, RequestKind, RequestPayload, ResponseEnvelope, ResponsePayload,
    RuntimeStatus, SshDynamicForward, SshHostProfile, SshTcpForward, WorkspaceSummary,
    API_VERSION,
};

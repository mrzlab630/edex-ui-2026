mod framing;
mod protocol;

pub use context_engine::ContextResult;
pub use file_index::{FileEntry, FilePreview, FileSearchResult};
pub use framing::{
    decode_json_frame, encode_json_frame, FrameAssumptions, FrameEncoding, FrameError,
    FRAME_CONTENT_TYPE, FRAME_DELIMITER, FRAME_DELIMITER_STR, LOCAL_TRANSPORT, MAX_FRAME_BYTES,
};
pub use protocol::{
    AgentProviderStatus, ApiError, Command, DaemonInfo, ErrorCode, EventEnvelope, HealthSnapshot,
    Query, RequestEnvelope, RequestKind, RequestPayload, ResponseEnvelope, ResponsePayload,
    RuntimeStatus, SshDynamicForward, SshHostProfile, SshTcpForward, API_VERSION,
};

//! nomic 领域模型与 trait 词汇。
//!
//! 本 crate 是全仓库的领域核心：消息、工具调用、会话树、事件、受理请求、
//! IPC 协议消息类型，以及存储、LLM 适配器、执行环境三个抽象 trait。
//! 不依赖 rig、不假设 SQLite，可被任何客户端复用。

pub mod error;
pub mod event;
pub mod exec;
pub mod id;
pub mod llm;
pub mod message;
pub mod protocol;
pub mod session;
pub mod step;
pub mod storage;
pub mod submission;
pub mod tool;
pub mod tool_registry;
pub mod tree;

pub use error::{EnvironmentError, LlmError, StorageError};
pub use event::{Event, EventErrorKind};
pub use exec::ExecutionEnvironment;
pub use id::{MessageId, RequestId, SessionId, ToolCallId};
pub use llm::{LlmAdapter, LlmEvent, LlmEventStream};
pub use message::{Message, Role};
pub use protocol::{ClientMessage, PROTOCOL_VERSION, ServerMessage};
pub use session::{ParentRef, Session, SessionState, StateTransitionError};
pub use step::Step;
pub use storage::Storage;
pub use submission::{Acceptance, SessionTarget, Submission};
pub use tool::{Tool, ToolCall, ToolError, ToolName, ToolResult, ToolSpec};
pub use tool_registry::{ToolRegistry, UnknownTool};
pub use tree::{SessionTree, SessionTreeSnapshot, TreeError};

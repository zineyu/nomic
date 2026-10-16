//! 领域 id 类型。
//!
//! 每类实体一个专用 newtype，外部无法把会话 id 当消息 id 使用；
//! 序列化为 UUID 字符串，可跨进程传输与落库。

use std::fmt;
use std::str::FromStr;

use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// 生成一个内部统一的 id newtype 定义，避免四处重复样板。
macro_rules! define_id {
    ($name:ident, $doc:literal) => {
        #[doc = $doc]
        #[derive(
            Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize,
        )]
        #[serde(transparent)]
        pub struct $name(Uuid);

        impl $name {
            /// 生成随机 id。
            pub fn new() -> Self {
                Self(Uuid::new_v4())
            }

            /// 从 UUID 构造（存储回放、测试注入确定性 id 时使用）。
            pub const fn from_uuid(uuid: Uuid) -> Self {
                Self(uuid)
            }

            /// 取底层 UUID。
            pub const fn as_uuid(&self) -> Uuid {
                self.0
            }
        }

        impl Default for $name {
            fn default() -> Self {
                Self::new()
            }
        }

        impl fmt::Display for $name {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                write!(f, "{}", self.0)
            }
        }

        impl FromStr for $name {
            type Err = uuid::Error;

            fn from_str(s: &str) -> Result<Self, Self::Err> {
                Uuid::parse_str(s).map(Self)
            }
        }
    };
}

define_id!(SessionId, "会话 id，标识会话树中的一个节点。");
define_id!(MessageId, "消息 id，标识一次会话中的一条消息。");
define_id!(ToolCallId, "工具调用 id，标识一次工具调用及其结果。");
define_id!(
    RequestId,
    "受理请求 id，客户端每次提交生成，服务端据此去重。"
);

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn new_id_每次生成不同值() {
        assert_ne!(SessionId::new(), SessionId::new());
        assert_ne!(MessageId::new(), MessageId::new());
        assert_ne!(ToolCallId::new(), ToolCallId::new());
        assert_ne!(RequestId::new(), RequestId::new());
    }

    #[test]
    fn display_与_from_str_往返一致() {
        let id = RequestId::new();
        let text = id.to_string();
        assert_eq!(text.parse::<RequestId>().unwrap(), id);
    }

    #[test]
    fn json_往返保持值() {
        let id = SessionId::new();
        let json = serde_json::to_string(&id).unwrap();
        assert_eq!(json, format!("\"{id}\""));
        assert_eq!(serde_json::from_str::<SessionId>(&json).unwrap(), id);
    }

    #[test]
    fn 非法字符串解析失败() {
        assert!("not-a-uuid".parse::<SessionId>().is_err());
    }
}

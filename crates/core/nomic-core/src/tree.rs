//! 会话树领域逻辑。
//!
//! 树持有会话元数据与各会话自有消息。分叉在分叉点（含）处复制
//! 消息前缀到新会话——写时复制语义的可执行表示：此后父子各自的
//! 追加互不可见。遍历按 [`SessionId`] 排序保证确定性。

use std::collections::BTreeMap;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::id::{MessageId, SessionId};
use crate::message::Message;
use crate::session::Session;

/// 会话树操作失败。
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum TreeError {
    /// 引用的会话不存在。
    #[error("session not found: {0}")]
    SessionMissing(SessionId),
    /// 分叉点消息不属于来源会话。
    #[error("fork point message not found in source session: {0}")]
    ForkPointMissing(MessageId),
}

/// 会话树中的一个节点：元数据 + 本会话自有消息。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct SessionNode {
    session: Session,
    /// 本会话的消息序列；分叉时已复制前缀，之后父子追加互不可见。
    messages: Vec<Message>,
}

/// 会话树快照（事件负载）：只含会话元数据，不含消息体。
///
/// `SessionTreeChanged` 事件携带本类型；消息体经
/// `Resume`/回放路径按需传输，事件流不带无界负载。
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct SessionTreeSnapshot {
    sessions: Vec<Session>,
}

impl SessionTreeSnapshot {
    /// 快照中的全部会话（按 id 排序）。
    #[must_use]
    pub fn sessions(&self) -> &[Session] {
        &self.sessions
    }

    /// 从会话元数据列表构造快照（存储实现读取路径专用）。
    ///
    /// 会话按 id 排序，与 [`SessionTree::snapshot`] 的输出形态一致。
    #[must_use]
    pub fn from_sessions(mut sessions: Vec<Session>) -> Self {
        sessions.sort_by_key(Session::id);
        Self { sessions }
    }

    /// 快照是否为空。
    #[must_use]
    pub const fn is_empty(&self) -> bool {
        self.sessions.is_empty()
    }

    /// 快照中的会话数量。
    #[must_use]
    pub const fn len(&self) -> usize {
        self.sessions.len()
    }
}

/// 会话树：全部会话、父子从属关系与各会话消息序列。
///
/// 分叉产生新节点并复制写时复制前缀；父子会话的后续追加
/// 互不可见。存储实现读取后据此渲染树形列表。
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct SessionTree {
    sessions: BTreeMap<SessionId, SessionNode>,
}

impl SessionTree {
    /// 空树。
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// 新建根会话并加入树。
    pub fn create_root(&mut self, working_dir: impl Into<PathBuf>) -> SessionId {
        let session = Session::root(working_dir.into());
        let id = session.id();
        self.sessions.insert(
            id,
            SessionNode {
                session,
                messages: Vec::new(),
            },
        );
        id
    }

    /// 向会话追加一条消息。
    ///
    /// # Errors
    ///
    /// 会话不存在时返回 [`TreeError::SessionMissing`]。
    pub fn append_message(
        &mut self,
        session: SessionId,
        message: Message,
    ) -> Result<(), TreeError> {
        let node = self
            .sessions
            .get_mut(&session)
            .ok_or(TreeError::SessionMissing(session))?;
        node.messages.push(message);
        Ok(())
    }

    /// 读取会话的消息序列（含分叉时复制的前缀）。
    ///
    /// # Errors
    ///
    /// 会话不存在时返回 [`TreeError::SessionMissing`]。
    pub fn messages(&self, session: SessionId) -> Result<&[Message], TreeError> {
        let node = self
            .sessions
            .get(&session)
            .ok_or(TreeError::SessionMissing(session))?;
        Ok(&node.messages)
    }

    /// 在 `source` 会话的 `at` 消息处分叉出新会话。
    ///
    /// 复制 `source` 消息序列中至 `at`（含）的前缀作为新会话的
    /// 初始序列；此后父子追加互不可见。新会话继承工作目录。
    ///
    /// # Errors
    ///
    /// 返回 [`TreeError::SessionMissing`] 当 `source` 不存在；
    /// 返回 [`TreeError::ForkPointMissing`] 当 `at` 不属于 `source`。
    pub fn fork(&mut self, source: SessionId, at: MessageId) -> Result<SessionId, TreeError> {
        let node = self
            .sessions
            .get(&source)
            .ok_or(TreeError::SessionMissing(source))?;
        let index = node
            .messages
            .iter()
            .position(|message| message.id() == at)
            .ok_or(TreeError::ForkPointMissing(at))?;
        let child = Session::forked_from(&node.session, at);
        let id = child.id();
        let node = SessionNode {
            session: child,
            messages: node.messages[..=index].to_vec(),
        };
        self.sessions.insert(id, node);
        Ok(id)
    }

    /// 取会话元数据。
    #[must_use]
    pub fn get(&self, id: SessionId) -> Option<&Session> {
        self.sessions.get(&id).map(|node| &node.session)
    }

    /// 根会话列表（无父节点的会话），按 id 排序。
    #[must_use]
    pub fn roots(&self) -> Vec<&Session> {
        self.sessions
            .values()
            .map(|node| &node.session)
            .filter(|session| session.parent().is_none())
            .collect()
    }

    /// 某会话的直接子会话，按 id 排序。
    #[must_use]
    pub fn children_of(&self, id: SessionId) -> Vec<&Session> {
        self.sessions
            .values()
            .map(|node| &node.session)
            .filter(|session| session.parent().is_some_and(|parent| parent.session == id))
            .collect()
    }

    /// 不含消息体的树快照，供事件负载使用。
    #[must_use]
    pub fn snapshot(&self) -> SessionTreeSnapshot {
        SessionTreeSnapshot {
            sessions: self
                .sessions
                .values()
                .map(|node| node.session.clone())
                .collect(),
        }
    }

    /// 会话总数。
    #[must_use]
    pub fn len(&self) -> usize {
        self.sessions.len()
    }

    /// 树是否为空。
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.sessions.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use super::*;
    use crate::session::SessionState;

    fn user_message(id: MessageId, text: &str) -> Message {
        Message::User {
            id,
            text: text.into(),
        }
    }

    #[test]
    fn 新树为空() {
        assert!(SessionTree::new().is_empty());
    }

    #[test]
    fn 根会话进入树并可按_id_取回() {
        let mut tree = SessionTree::new();
        let id = tree.create_root("/tmp/ws");
        assert_eq!(tree.len(), 1);
        assert_eq!(tree.get(id).unwrap().id(), id);
        assert_eq!(tree.roots(), vec![tree.get(id).unwrap()]);
        assert!(tree.messages(id).unwrap().is_empty());
    }

    #[test]
    fn 向不存在会话追加消息返回错误() {
        let mut tree = SessionTree::new();
        let result = tree.append_message(SessionId::new(), user_message(MessageId::new(), "m"));
        assert!(matches!(result, Err(TreeError::SessionMissing(_))));
    }

    #[test]
    fn 从不存在会话分叉返回错误() {
        let mut tree = SessionTree::new();
        let result = tree.fork(SessionId::new(), MessageId::new());
        assert!(matches!(result, Err(TreeError::SessionMissing(_))));
    }

    #[test]
    fn 对空会话或未知消息分叉返回分叉点错误() {
        let mut tree = SessionTree::new();
        let root = tree.create_root("/tmp/ws");
        // 空会话没有任何消息可作分叉点
        assert!(matches!(
            tree.fork(root, MessageId::new()),
            Err(TreeError::ForkPointMissing(_))
        ));
        let message = MessageId::new();
        tree.append_message(root, user_message(message, "m1"))
            .unwrap();
        // 分叉点不属于来源会话
        assert!(matches!(
            tree.fork(root, MessageId::new()),
            Err(TreeError::ForkPointMissing(_))
        ));
        // 分叉点属于其他会话同样拒绝
        let other = tree.create_root("/tmp/other");
        assert!(matches!(
            tree.fork(other, message),
            Err(TreeError::ForkPointMissing(_))
        ));
    }

    #[test]
    fn 分叉复制至分叉点含的前缀() {
        let mut tree = SessionTree::new();
        let root = tree.create_root("/tmp/ws");
        let m1 = MessageId::new();
        let m2 = MessageId::new();
        let m3 = MessageId::new();
        for (id, text) in [(m1, "m1"), (m2, "m2"), (m3, "m3")] {
            tree.append_message(root, user_message(id, text)).unwrap();
        }
        let child = tree.fork(root, m2).unwrap();
        let child_messages: Vec<MessageId> = tree
            .messages(child)
            .unwrap()
            .iter()
            .map(Message::id)
            .collect();
        assert_eq!(child_messages, vec![m1, m2]);
    }

    #[test]
    fn 分叉后父子各自追加互不可见() {
        let mut tree = SessionTree::new();
        let root = tree.create_root("/tmp/ws");
        let m1 = MessageId::new();
        tree.append_message(root, user_message(m1, "m1")).unwrap();
        let child = tree.fork(root, m1).unwrap();

        // 分叉后父会话追加，子会话不可见
        let m2 = MessageId::new();
        tree.append_message(root, user_message(m2, "root only"))
            .unwrap();
        let child_ids: Vec<MessageId> = tree
            .messages(child)
            .unwrap()
            .iter()
            .map(Message::id)
            .collect();
        assert_eq!(child_ids, vec![m1]);

        // 子会话追加，父会话不可见
        let c1 = MessageId::new();
        tree.append_message(child, user_message(c1, "child only"))
            .unwrap();
        let root_ids: Vec<MessageId> = tree
            .messages(root)
            .unwrap()
            .iter()
            .map(Message::id)
            .collect();
        assert_eq!(root_ids, vec![m1, m2]);
        let child_ids: Vec<MessageId> = tree
            .messages(child)
            .unwrap()
            .iter()
            .map(Message::id)
            .collect();
        assert_eq!(child_ids, vec![m1, c1]);
    }

    #[test]
    fn 同一分叉点可分出前缀相同的兄弟会话() {
        let mut tree = SessionTree::new();
        let root = tree.create_root("/tmp/ws");
        let m1 = MessageId::new();
        tree.append_message(root, user_message(m1, "m1")).unwrap();
        let left = tree.fork(root, m1).unwrap();
        let right = tree.fork(root, m1).unwrap();
        assert_eq!(tree.messages(left).unwrap(), tree.messages(right).unwrap());
        assert_ne!(left, right);
    }

    #[test]
    fn 分叉记录父子关系且树形列表正确() {
        let mut tree = SessionTree::new();
        let root = tree.create_root("/tmp/ws");
        let m1 = MessageId::new();
        let m2 = MessageId::new();
        tree.append_message(root, user_message(m1, "m1")).unwrap();
        tree.append_message(root, user_message(m2, "m2")).unwrap();
        let child = tree.fork(root, m1).unwrap();
        let grandchild = tree.fork(child, m1).unwrap();

        assert_eq!(tree.len(), 3);
        assert_eq!(tree.roots().len(), 1);
        assert_eq!(tree.children_of(root).len(), 1);
        assert_eq!(tree.children_of(child).len(), 1);
        assert_eq!(tree.children_of(grandchild).len(), 0);
        assert_eq!(tree.get(child).unwrap().parent().unwrap().session, root);
        assert_eq!(
            tree.get(grandchild).unwrap().parent().unwrap().session,
            child
        );
    }

    #[test]
    fn 多根会话并列存在() {
        let mut tree = SessionTree::new();
        let a = tree.create_root("/tmp/a");
        let b = tree.create_root("/tmp/b");
        let roots = tree.roots();
        assert_eq!(roots.len(), 2);
        assert!(roots.iter().any(|s| s.id() == a));
        assert!(roots.iter().any(|s| s.id() == b));
    }

    #[test]
    fn from_sessions_按_id_排序且保留全部会话() {
        let mut tree = SessionTree::new();
        let root = tree.create_root("/tmp/ws");
        let m1 = MessageId::new();
        tree.append_message(root, user_message(m1, "m1")).unwrap();
        let child = tree.fork(root, m1).unwrap();
        let unordered: Vec<Session> = [child, root]
            .into_iter()
            .filter_map(|id| tree.get(id).cloned())
            .collect();
        let snapshot = SessionTreeSnapshot::from_sessions(unordered);
        let ids: Vec<SessionId> = snapshot.sessions().iter().map(Session::id).collect();
        let mut sorted = ids.clone();
        sorted.sort();
        assert_eq!(ids, sorted);
        assert_eq!(snapshot.len(), 2);
    }

    #[test]
    fn 快照只含元数据且按_id_排序() {
        let mut tree = SessionTree::new();
        let root = tree.create_root("/tmp/ws");
        let m1 = MessageId::new();
        tree.append_message(root, user_message(m1, "m1")).unwrap();
        tree.fork(root, m1).unwrap();

        let snapshot = tree.snapshot();
        assert_eq!(snapshot.len(), 2);
        assert!(!snapshot.is_empty());
        let ids: Vec<SessionId> = snapshot.sessions().iter().map(Session::id).collect();
        let mut sorted = ids;
        sorted.sort();
        assert_eq!(
            snapshot
                .sessions()
                .iter()
                .map(Session::id)
                .collect::<Vec<_>>(),
            sorted
        );
        // 快照可序列化（事件负载），且不含消息体字段
        let json = serde_json::to_value(&snapshot).unwrap();
        assert!(json.to_string().contains("working_dir"));
        assert!(!json.to_string().contains("m1"));
    }

    #[test]
    fn 树json_往返保持值_含消息() {
        let mut tree = SessionTree::new();
        let root = tree.create_root("/tmp/ws");
        let m1 = MessageId::new();
        tree.append_message(root, user_message(m1, "m1")).unwrap();
        tree.fork(root, m1).unwrap();
        let json = serde_json::to_string(&tree).unwrap();
        assert_eq!(serde_json::from_str::<SessionTree>(&json).unwrap(), tree);
    }

    #[test]
    fn 分叉的子会话继承工作目录与初始静止状态() {
        let mut tree = SessionTree::new();
        let root = tree.create_root("/tmp/ws");
        let m1 = MessageId::new();
        tree.append_message(root, user_message(m1, "m1")).unwrap();
        let child = tree.fork(root, m1).unwrap();
        let child = tree.get(child).unwrap();
        assert_eq!(child.working_dir(), Path::new("/tmp/ws"));
        assert_eq!(child.state(), SessionState::Idle);
    }
}

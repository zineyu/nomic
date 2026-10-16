//! 会话树领域逻辑。
//!
//! 纯内存结构，按 [`SessionId`] 排序保证遍历确定性。树只维护
//! 父子结构；分叉点的消息归属由存储层校验（树不持有消息）。

use std::collections::BTreeMap;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::id::{MessageId, SessionId};
use crate::session::Session;

/// 会话树操作失败。
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum TreeError {
    /// 引用的会话不存在。
    #[error("session not found: {0}")]
    SessionMissing(SessionId),
}

/// 会话树：全部会话及其父子从属关系。
///
/// 存储实现读取后据此渲染树形列表；分叉产生新节点，
/// 父子会话的后续追加互不可见（各自的消息序列独立）。
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct SessionTree {
    sessions: BTreeMap<SessionId, Session>,
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
        self.sessions.insert(id, session);
        id
    }

    /// 在 `source` 会话的 `at` 消息处分叉出新会话。
    ///
    /// 新会话继承工作目录，父子关系写入树；写时复制的消息前缀
    /// 由存储层落库。`at` 是否属于 `source` 的校验同样属于存储层。
    ///
    /// # Errors
    ///
    /// 返回 [`TreeError::SessionMissing`] 当 `source` 不存在。
    pub fn fork(&mut self, source: SessionId, at: MessageId) -> Result<SessionId, TreeError> {
        let parent = self
            .sessions
            .get(&source)
            .ok_or(TreeError::SessionMissing(source))?;
        let child = Session::forked_from(parent, at);
        let id = child.id();
        self.sessions.insert(id, child);
        Ok(id)
    }

    /// 取会话。
    #[must_use]
    pub fn get(&self, id: SessionId) -> Option<&Session> {
        self.sessions.get(&id)
    }

    /// 根会话列表（无父节点的会话），按 id 排序。
    #[must_use]
    pub fn roots(&self) -> Vec<&Session> {
        self.sessions
            .values()
            .filter(|session| session.parent().is_none())
            .collect()
    }

    /// 某会话的直接子会话，按 id 排序。
    #[must_use]
    pub fn children_of(&self, id: SessionId) -> Vec<&Session> {
        self.sessions
            .values()
            .filter(|session| session.parent().is_some_and(|parent| parent.session == id))
            .collect()
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

    /// 按 id 遍历全部会话（排序确定）。
    pub fn iter(&self) -> impl Iterator<Item = &Session> {
        self.sessions.values()
    }
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use super::*;
    use crate::session::SessionState;

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
    }

    #[test]
    fn 从不存在会话分叉返回错误() {
        let mut tree = SessionTree::new();
        let result = tree.fork(SessionId::new(), MessageId::new());
        assert!(matches!(result, Err(TreeError::SessionMissing(_))));
    }

    #[test]
    fn 分叉记录父子关系且树形列表正确() {
        let mut tree = SessionTree::new();
        let root = tree.create_root("/tmp/ws");
        let child = tree.fork(root, MessageId::new()).unwrap();
        let grandchild = tree.fork(child, MessageId::new()).unwrap();

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
    fn 分叉后父子各自追加互不可见() {
        let mut tree = SessionTree::new();
        let root = tree.create_root("/tmp/ws");
        let child = tree.fork(root, MessageId::new()).unwrap();
        let sibling = tree.fork(root, MessageId::new()).unwrap();

        // 树层面互不可见：子会话的 children 不混入兄弟分支
        let child_ids: Vec<_> = tree.children_of(root).iter().map(|s| s.id()).collect();
        assert!(child_ids.contains(&child));
        assert!(child_ids.contains(&sibling));
        assert_eq!(tree.children_of(child).len(), 0);
        assert_ne!(
            tree.get(child).unwrap().id(),
            tree.get(sibling).unwrap().id()
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
    fn 树json_往返保持值() {
        let mut tree = SessionTree::new();
        let root = tree.create_root("/tmp/ws");
        tree.fork(root, MessageId::new()).unwrap();
        let json = serde_json::to_string(&tree).unwrap();
        assert_eq!(serde_json::from_str::<SessionTree>(&json).unwrap(), tree);
    }

    #[test]
    fn 分叉的子会话继承工作目录与初始静止状态() {
        let mut tree = SessionTree::new();
        let root = tree.create_root("/tmp/ws");
        let child = tree.fork(root, MessageId::new()).unwrap();
        let child = tree.get(child).unwrap();
        assert_eq!(child.working_dir(), Path::new("/tmp/ws"));
        assert_eq!(child.state(), SessionState::Idle);
    }
}

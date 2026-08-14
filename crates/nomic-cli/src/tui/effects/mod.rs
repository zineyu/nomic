//! Effect 执行逻辑：按 Effect 族分组，`tui::execute_effect` 只做转发。
//!
//! - [`model`]：模型 + 思考级别两步流（`/models` 候选与选择器、待切换模型
//!   暂存、级别应用与选择落库）
//! - [`session`]：会话管理（`/resume` 恢复、`/tree` 浏览与分支、`/new` 新建）；
//!   会话落库绑定（recorder + cwd）收在本模块的 [`SessionBinding`]，driver
//!   只持有实例——事件落库一行接线，session 切换时的 recorder 换绑方法化
//!   （定稿点落库收在 `nomic_session::SessionRecorder`）
//! - [`clipboard`]：剪贴板与图片暂存（bracketed paste、Ctrl+V 粘贴、`/copy`、
//!   `/image` 与 `--image` 附件）

mod clipboard;
mod model;
mod session;

pub(super) use clipboard::{
    attach_image, copy_to_clipboard, handle_paste, paste_clipboard, stage_cli_images,
};
pub(super) use model::{cancel_model_switch, list_models, select_model, set_reasoning};
pub(super) use session::{
    SessionBinding, branch_to, list_sessions, list_tree, new_session, resume_session,
};

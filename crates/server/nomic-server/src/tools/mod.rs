//! 内置工具集：读文件、写文件、shell 执行、块替换编辑。
//!
//! 坐标系：相对路径经会话工作目录解释（派发时由执行环境传入），
//! 绝对路径原样使用。replay-safe 声明：只读工具（read）为真；
//! write / bash / edit 重复执行有副作用，为假。

mod bash;
mod edit;
mod read;
mod write;

use std::path::{Path, PathBuf};

use nomic_core::ToolRegistry;

pub use bash::{Bash, BashInput, BashOutput};
pub use edit::{Edit, EditInput, EditOutput};
pub use read::{Read, ReadInput, ReadOutput};
pub use write::{Write, WriteInput, WriteOutput};

/// 组装内置工具注册表：四工具按名称派发，声明含 replay-safe 标记。
pub fn builtin_registry() -> ToolRegistry {
    let mut registry = ToolRegistry::new();
    registry.register(Read);
    registry.register(Write);
    registry.register(Bash);
    registry.register(Edit);
    registry
}

/// 路径坐标解释：绝对路径原样使用，相对路径基于工作目录拼接。
pub(crate) fn resolve_path(working_dir: &Path, raw: &str) -> PathBuf {
    let path = Path::new(raw);
    if path.is_absolute() {
        path.to_path_buf()
    } else {
        working_dir.join(path)
    }
}

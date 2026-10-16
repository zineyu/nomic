//! nomic 终端客户端。
//!
//! 经 Unix socket + JSON Lines 协议连接 server，渲染会话树、流式输出与
//! 对话历史，负责输入编辑与 Esc 取消。本体在 S9 落地。

#[cfg(test)]
mod tests {
    #[test]
    fn placeholder() {}
}

//! nomic 常驻 server。
//!
//! 由受理（requestId 去重）、调度器（并发上限、排队）、worker 池组成；
//! 会话执行逻辑为调度重驱状态机。各组件在后续切片落地。

#[cfg(test)]
mod tests {
    #[test]
    fn placeholder() {}
}

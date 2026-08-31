//! 内部 URI 路由器：scheme → [`ProtocolHandler`] 注册表。
//!
//! 与 oh-my-pi 的进程全局单例不同，nomic **按会话构建** router 实例
//!（工具本就按会话构造注入），handler 的后端在构造期注入，因此无需
//! 逐次调用的上下文修正层。使用方（read/write/grep/bash 工具）共享
//! `Arc<UriRouter>`。

use std::collections::HashMap;
use std::sync::Arc;

use crate::handler::{ProtocolHandler, UriError, UriResource};
use crate::parse::{InternalUri, extract_uri_scheme, hierarchical_scheme, parse_internal_uri};

/// 内部 URI 路由器。
#[derive(Default)]
pub struct UriRouter {
    handlers: HashMap<String, Arc<dyn ProtocolHandler>>,
}

impl std::fmt::Debug for UriRouter {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("UriRouter")
            .field("schemes", &self.handlers.keys().collect::<Vec<_>>())
            .finish_non_exhaustive()
    }
}

impl UriRouter {
    /// 空路由表。
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// 注册 handler（同 scheme 后注册覆盖先注册）。
    pub fn register(&mut self, handler: Arc<dyn ProtocolHandler>) {
        self.handlers.insert(handler.scheme().to_string(), handler);
    }

    /// 取 scheme 对应的 handler。
    #[must_use]
    pub fn handler(&self, scheme: &str) -> Option<Arc<dyn ProtocolHandler>> {
        self.handlers.get(&scheme.to_ascii_lowercase()).cloned()
    }

    /// 输入是否为已注册 scheme 的层级式内部 URI（`scheme://…`）。
    #[must_use]
    pub fn can_handle(&self, input: &str) -> bool {
        hierarchical_scheme(input).is_some_and(|scheme| self.handlers.contains_key(&scheme))
    }

    /// read 是否可解析该输入。
    ///
    /// 与 oh-my-pi 的 MCP 兜底不同，nomic 当前没有「任意自定义 scheme」的
    /// 外部资源源，因此 `can_resolve` 等价于 `can_handle`；opaque URI 形
    /// 输入一律不可解析。
    #[must_use]
    pub fn can_resolve(&self, input: &str) -> bool {
        self.can_handle(input)
    }

    /// 输入是否形似 URI（无论 scheme 是否注册）。write 用它把
    /// 「形似但未注册」的目标与「纯文件路径」区分开：前者报错纠错，
    /// 后者走文件系统。
    #[must_use]
    pub fn looks_like_uri(input: &str) -> bool {
        extract_uri_scheme(input).is_some()
    }

    /// 解析并分发，返回盖过 immutable 章的资源。
    pub async fn resolve(&self, input: &str) -> Result<UriResource, UriError> {
        let (url, handler) = self.route(input)?;
        let mut resource = handler.resolve(&url).await?;
        resource.immutable = Some(resource.immutable.unwrap_or_else(|| handler.immutable()));
        Ok(resource)
    }

    /// 写入分发；无 `write` 实现的 scheme 报只读错误（结构性只读）。
    pub async fn write(&self, input: &str, content: &str) -> Result<(), UriError> {
        let (url, handler) = self.route(input)?;
        if !handler.writable() {
            return Err(UriError::ReadOnly { scheme: url.scheme });
        }
        handler.write(&url, content).await
    }

    /// 支持补全的 scheme 列表（升序）。
    #[must_use]
    pub fn completion_schemes(&self) -> Vec<&str> {
        let mut schemes: Vec<&str> = self
            .handlers
            .values()
            .filter(|handler| handler.supports_completion())
            .map(|handler| handler.scheme())
            .collect();
        schemes.sort_unstable();
        schemes
    }

    /// 路由：解析 + 查表；未知 scheme 报附带可用列表的错误。
    fn route(&self, input: &str) -> Result<(InternalUri, Arc<dyn ProtocolHandler>), UriError> {
        let url = parse_internal_uri(input)?;
        let Some(handler) = self.handlers.get(&url.scheme) else {
            let supported = self
                .handlers
                .keys()
                .map(|scheme| format!("{scheme}://"))
                .collect::<Vec<_>>()
                .join(", ");
            return Err(UriError::UnknownScheme {
                scheme: url.scheme,
                supported,
            });
        };
        Ok((url, handler.clone()))
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use async_trait::async_trait;

    use crate::handler::{ProtocolHandler, UriError, UriResource};
    use crate::parse::InternalUri;
    use crate::router::UriRouter;

    struct StaticHandler {
        scheme: &'static str,
        immutable: bool,
        content: String,
        /// 单个资源级覆盖（Some 时优先于 handler 默认值）
        resource_immutable: Option<bool>,
    }

    #[async_trait]
    impl ProtocolHandler for StaticHandler {
        fn scheme(&self) -> &'static str {
            self.scheme
        }
        fn immutable(&self) -> bool {
            self.immutable
        }
        async fn resolve(&self, url: &InternalUri) -> Result<UriResource, UriError> {
            let mut resource = UriResource::text(url.raw_href.clone(), self.content.clone());
            resource.immutable = self.resource_immutable;
            Ok(resource)
        }
    }

    struct WritableHandler;

    #[async_trait]
    impl ProtocolHandler for WritableHandler {
        fn scheme(&self) -> &'static str {
            "local"
        }
        fn immutable(&self) -> bool {
            false
        }
        async fn resolve(&self, url: &InternalUri) -> Result<UriResource, UriError> {
            Ok(UriResource::text(url.raw_href.clone(), "sandbox"))
        }
        fn writable(&self) -> bool {
            true
        }
        async fn write(&self, _url: &InternalUri, _content: &str) -> Result<(), UriError> {
            Ok(())
        }
    }

    fn router() -> UriRouter {
        let mut router = UriRouter::new();
        router.register(Arc::new(StaticHandler {
            scheme: "skill",
            immutable: true,
            content: "skill body".into(),
            resource_immutable: None,
        }));
        router.register(Arc::new(WritableHandler));
        router
    }

    #[test]
    fn can_handle_only_registered_hierarchical() {
        let router = router();
        assert!(router.can_handle("skill://pdf"));
        assert!(router.can_handle("SKILL://pdf")); // scheme 大小写不敏感
        assert!(!router.can_handle("missing://x"));
        assert!(!router.can_handle("skill:opaque")); // opaque 形式不可路由
        assert!(!router.can_handle("src/main.rs"));
        assert!(router.can_resolve("local://a.md"));
        assert!(UriRouter::looks_like_uri("missing://x"));
        assert!(!UriRouter::looks_like_uri("src/main.rs"));
    }

    #[tokio::test]
    async fn resolve_stamps_handler_immutable_default() {
        let router = router();
        let resource = router.resolve("skill://pdf").await.expect("resolve");
        assert!(resource.is_immutable()); // handler 默认 true
        assert_eq!(resource.content, "skill body");

        let resource = router.resolve("local://a.md").await.expect("resolve");
        assert!(!resource.is_immutable()); // handler 默认 false
    }

    #[tokio::test]
    async fn resource_level_immutable_overrides_default() {
        let mut router = UriRouter::new();
        router.register(Arc::new(StaticHandler {
            scheme: "vault",
            immutable: false,
            content: "listing".into(),
            resource_immutable: Some(true), // 目录清单类派生内容覆盖为不可变
        }));
        let resource = router.resolve("vault://work/").await.expect("resolve");
        assert!(resource.is_immutable());
    }

    #[tokio::test]
    async fn write_routes_to_writable_handler_only() {
        let router = router();
        router
            .write("local://a.md", "content")
            .await
            .expect("write");

        let error = router.write("skill://pdf", "content").await.unwrap_err();
        assert!(error.to_string().contains("read-only"), "{error}");
    }

    #[tokio::test]
    async fn unknown_scheme_error_lists_supported() {
        let router = router();
        let error = router.resolve("bogus://x").await.unwrap_err();
        let message = error.to_string();
        assert!(message.contains("Unknown protocol: bogus://"), "{message}");
        assert!(message.contains("skill://"), "{message}");
        assert!(message.contains("local://"), "{message}");
    }

    #[tokio::test]
    async fn invalid_uri_is_rejected() {
        let router = router();
        assert!(router.resolve("not a uri").await.is_err());
    }
}

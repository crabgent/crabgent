use std::sync::Arc;

use crabgent_core::{Kernel, Tool};

use crate::config::McpServerConfig;
use crate::error::McpServerError;
use crate::session::McpSessionRegistry;
use crate::tools::chat::CHAT_TOOL_NAME;

pub type ToolFilter = Arc<dyn Fn(&str) -> bool + Send + Sync>;

#[derive(Default)]
pub struct McpServerBuilder {
    kernel: Option<Arc<Kernel>>,
    config: Option<McpServerConfig>,
    tool_filter: Option<ToolFilter>,
}

impl McpServerBuilder {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    #[must_use]
    pub fn with_kernel(mut self, kernel: Arc<Kernel>) -> Self {
        self.kernel = Some(kernel);
        self
    }

    #[must_use]
    pub fn with_config(mut self, config: McpServerConfig) -> Self {
        self.config = Some(config);
        self
    }

    #[must_use]
    pub fn with_tool_filter(
        mut self,
        filter: impl Fn(&str) -> bool + Send + Sync + 'static,
    ) -> Self {
        self.tool_filter = Some(Arc::new(filter));
        self
    }

    pub fn build(self) -> Result<McpServer, McpServerError> {
        let kernel = self
            .kernel
            .ok_or_else(|| McpServerError::InvalidRequest("kernel is required".into()))?;
        let config = self
            .config
            .ok_or_else(|| McpServerError::InvalidRequest("config is required".into()))?;
        let session_registry = McpSessionRegistry::new(config.max_sessions)?;

        Ok(McpServer {
            kernel,
            tool_filter: self.tool_filter.unwrap_or_else(default_tool_filter),
            config,
            session_registry,
        })
    }
}

pub struct McpServer {
    pub(crate) kernel: Arc<Kernel>,
    pub(crate) config: McpServerConfig,
    pub(crate) tool_filter: ToolFilter,
    pub(crate) session_registry: McpSessionRegistry,
}

impl McpServer {
    #[must_use]
    pub fn builder() -> McpServerBuilder {
        McpServerBuilder::new()
    }

    #[must_use]
    pub const fn kernel(&self) -> &Arc<Kernel> {
        &self.kernel
    }

    #[must_use]
    pub const fn config(&self) -> &McpServerConfig {
        &self.config
    }

    #[must_use]
    pub fn exposes_tool(&self, name: &str) -> bool {
        (self.tool_filter)(name)
    }

    #[must_use]
    pub(crate) fn exposes_chat_tool(&self) -> bool {
        self.exposes_tool(CHAT_TOOL_NAME)
    }

    pub(crate) fn visible_kernel_tools(&self) -> impl Iterator<Item = &Arc<dyn Tool>> {
        self.kernel
            .tools()
            .iter()
            .filter(|tool| self.exposes_tool(tool.name()))
    }

    pub(crate) fn visible_kernel_tool(&self, name: &str) -> Option<&Arc<dyn Tool>> {
        self.visible_kernel_tools().find(|tool| tool.name() == name)
    }

    #[must_use]
    pub const fn session_capacity(&self) -> usize {
        self.session_registry.max_sessions()
    }
}

fn default_tool_filter() -> ToolFilter {
    Arc::new(|_| true)
}

#[cfg(test)]
mod tests {
    use crabgent_core::{AllowAllPolicy, Kernel, ModelInfo, ModelTarget};
    use crabgent_test_support::StubProvider;
    use secrecy::SecretString;

    use super::*;

    fn test_provider() -> StubProvider {
        StubProvider::new()
            .with_name("test-provider")
            .with_models(vec![ModelInfo::minimal("test-model", "test-provider")])
    }

    fn test_kernel() -> Arc<Kernel> {
        Arc::new(
            Kernel::builder()
                .provider(test_provider())
                .policy(AllowAllPolicy)
                .try_build()
                .expect("test provider advertises one valid model"),
        )
    }

    fn test_config() -> McpServerConfig {
        McpServerConfig::new(
            SecretString::from("secret-test-token-12345"),
            ModelTarget::id("test-model"),
        )
    }

    #[test]
    fn builder_creates_server() {
        let server = McpServerBuilder::new()
            .with_kernel(test_kernel())
            .with_config(test_config().with_max_sessions(7))
            .with_tool_filter(|name| name == "chat")
            .build()
            .expect("kernel and config are present");

        assert_eq!(server.config().max_sessions, 7);
        assert_eq!(server.session_capacity(), 7);
        assert_eq!(server.kernel().provider_name(), "test-provider");
        assert!(server.exposes_tool("chat"));
        assert!(!server.exposes_tool("blocked"));
    }

    #[test]
    fn missing_kernel_errors() {
        let error = McpServerBuilder::new()
            .with_config(test_config())
            .build()
            .err()
            .expect("missing kernel must fail");

        assert!(matches!(error, McpServerError::InvalidRequest(_)));
        assert!(error.to_string().contains("kernel"));
    }

    #[test]
    fn missing_config_errors() {
        let error = McpServerBuilder::new()
            .with_kernel(test_kernel())
            .build()
            .err()
            .expect("missing config must fail");

        assert!(matches!(error, McpServerError::InvalidRequest(_)));
        assert!(error.to_string().contains("config"));
    }
}

use std::collections::HashSet;

use crate::McpError;
use crate::config::{McpServerConfig, validate_server_name};

use super::McpClient;

pub struct McpClientBuilder {
    configs: Vec<McpServerConfig>,
    names: HashSet<String>,
}

impl McpClientBuilder {
    pub fn new() -> Self {
        Self {
            configs: Vec::new(),
            names: HashSet::new(),
        }
    }

    pub fn add_server(&mut self, config: McpServerConfig) -> Result<&mut Self, McpError> {
        validate_server_name(&config.name)?;

        if !self.names.insert(config.name.clone()) {
            return Err(McpError::InvalidConfig(format!(
                "duplicate MCP server name '{}'",
                config.name
            )));
        }

        self.configs.push(config);
        Ok(self)
    }

    pub fn add_servers(
        &mut self,
        configs: impl IntoIterator<Item = McpServerConfig>,
    ) -> Result<&mut Self, McpError> {
        for config in configs {
            self.add_server(config)?;
        }
        Ok(self)
    }

    pub fn build(self) -> Result<Vec<(String, McpClient)>, McpError> {
        self.configs
            .into_iter()
            .map(|config| {
                let name = config.name.clone();
                McpClient::new(config).map(|client| (name, client))
            })
            .collect()
    }
}

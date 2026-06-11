use std::sync::Arc;

use crate::{McpClientBuilder, McpServerConfig, McpToolFactory};

#[crabgent_log::instrument(skip(configs), fields(count = configs.len()))]
pub async fn discover_servers(configs: &[McpServerConfig]) -> Vec<McpToolFactory> {
    let mut factories = Vec::new();
    let mut builder = McpClientBuilder::new();

    for config in configs {
        if let Err(err) = builder.add_server(config.clone()) {
            warn_discovery_failed(&config.name, &err);
        }
    }

    let clients = match builder.build() {
        Ok(clients) => clients,
        Err(err) => {
            warn_discovery_failed("<all>", &err);
            return factories;
        }
    };

    for (server_name, client) in clients {
        let client = Arc::new(client);
        match client.discover().await {
            Ok(defs) => {
                match McpToolFactory::from_client(
                    &server_name,
                    defs.tools,
                    &client,
                    client.max_output_bytes(),
                ) {
                    Ok(factory) => factories.push(factory),
                    Err(err) => warn_discovery_failed(&server_name, &err),
                }
            }
            Err(err) => warn_discovery_failed(&server_name, &err),
        }
    }

    factories
}

fn warn_discovery_failed(server: &str, err: &impl std::fmt::Display) {
    crabgent_log::warn!(
        server = %server,
        error = %crabgent_log::redact_text(&err.to_string()),
        "MCP server discovery failed - skipped"
    );
}

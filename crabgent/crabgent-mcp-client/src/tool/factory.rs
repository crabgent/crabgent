use std::collections::HashSet;
use std::sync::Arc;

use crabgent_core::Tool;

use crate::{McpClient, McpError, McpToolDef};

use super::McpTool;

pub struct McpToolFactory {
    tools: Vec<Arc<dyn Tool>>,
}

impl McpToolFactory {
    /// Build tools for a discovered MCP server.
    ///
    /// Leaks are bounded only when discovery is a one-time startup operation
    /// per server. Repeated factory initialization leaks proportionally to each
    /// invocation's metadata size.
    pub fn from_client(
        server_name: &str,
        defs: Vec<McpToolDef>,
        client: &Arc<McpClient>,
        max_output_bytes: usize,
    ) -> Result<Self, McpError> {
        let mut seen = HashSet::with_capacity(defs.len());
        let mut tools = Vec::with_capacity(defs.len());

        for def in defs {
            if !seen.insert(def.name.clone()) {
                return Err(McpError::InvalidConfig(format!(
                    "duplicate MCP tool name '{}'",
                    def.name
                )));
            }

            let prefixed_name = leak(prefixed_name(server_name, &def.name));
            let description = leak(description_with_cap(&def.description, max_output_bytes));
            let tool = McpTool {
                prefixed_name,
                original_name: def.name,
                description,
                input_schema: def.input_schema,
                client: Arc::clone(client),
                max_output_bytes,
            };
            tools.push(Arc::new(tool) as Arc<dyn Tool>);
        }

        Ok(Self { tools })
    }

    pub fn into_tools(self) -> Vec<Arc<dyn Tool>> {
        self.tools
    }
}

fn prefixed_name(server: &str, tool: &str) -> String {
    format!("{server}__{tool}")
}

fn description_with_cap(description: &str, max_output_bytes: usize) -> String {
    format!("{description}\n\nOutput capped at {max_output_bytes} bytes; default cap is 5 MB.")
}

/// Leak `value` as a `&'static str` for use in static metadata slots
/// (`Tool::name`, `Tool::description`).
///
/// `Box::leak` is intentional here: factory build happens once at startup,
/// the resulting `Vec<Arc<dyn Tool>>` lives for the process lifetime, so the
/// leaked bytes are never recovered anyway. The alternative is a per-tool
/// `OnceLock<String>` plus an extra borrow at every `Tool` method call,
/// which adds heap churn and code without changing the steady-state memory
/// profile. Switch to `OnceLock` if tools ever become dynamically rebuilt.
fn leak(value: String) -> &'static str {
    Box::leak(value.into_boxed_str())
}

#[cfg(test)]
mod tests {
    use super::prefixed_name;

    #[test]
    fn factory_prefixed_name_uses_double_underscore() {
        assert_eq!(prefixed_name("sg42", "search_docs"), "sg42__search_docs");
    }
}

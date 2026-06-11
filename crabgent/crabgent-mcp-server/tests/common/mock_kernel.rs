#![allow(
    dead_code,
    reason = "shared integration-test fixtures are compiled per test binary"
)]

use std::sync::Arc;

use async_trait::async_trait;
use crabgent_core::{AllowAllPolicy, DenyAllPolicy, Kernel, ModelInfo, Tool, ToolCtx, ToolError};
use crabgent_test_support::StubProvider;
use serde_json::{Value, json};

pub const MOCK_MODEL: &str = "mock-model";
const MOCK_PROVIDER: &str = "mock-provider";

fn mock_provider() -> StubProvider {
    StubProvider::with_text("mock-reply")
        .with_name(MOCK_PROVIDER)
        .with_tools(true)
        .with_models(vec![ModelInfo::minimal(MOCK_MODEL, MOCK_PROVIDER)])
}

struct MockTool;

#[async_trait]
impl Tool for MockTool {
    fn name(&self) -> &'static str {
        "mock_echo"
    }

    fn description(&self) -> &'static str {
        "Echo test tool"
    }

    fn parameters_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "text": { "type": "string" },
            },
            "required": ["text"],
        })
    }

    async fn execute(&self, args: Value, _ctx: &ToolCtx) -> Result<Value, ToolError> {
        let text = args
            .get("text")
            .and_then(Value::as_str)
            .ok_or_else(|| ToolError::InvalidArgs("mock_echo requires text string".into()))?;
        Ok(Value::String(format!("tool:{text}")))
    }
}

pub fn build_test_kernel() -> Arc<Kernel> {
    Arc::new(
        Kernel::builder()
            .provider(mock_provider())
            .policy(AllowAllPolicy)
            .add_tool(MockTool)
            .try_build()
            .expect("mock provider advertises one valid model"),
    )
}

pub fn build_denied_test_kernel() -> Arc<Kernel> {
    Arc::new(
        Kernel::builder()
            .provider(mock_provider())
            .policy(DenyAllPolicy)
            .add_tool(MockTool)
            .try_build()
            .expect("mock provider advertises one valid model"),
    )
}

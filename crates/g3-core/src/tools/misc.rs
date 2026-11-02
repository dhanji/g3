//! Miscellaneous tools: code_search.

use anyhow::Result;
use tracing::debug;

use crate::ui_writer::UiWriter;
use crate::ToolCall;

use super::executor::ToolContext;

/// Execute the `code_search` tool.
pub async fn execute_code_search<W: UiWriter>(
    tool_call: &ToolCall,
    _ctx: &ToolContext<'_, W>,
) -> Result<String> {
    debug!("Processing code_search tool call");

    // Parse the request
    let request: crate::code_search::CodeSearchRequest =
        match serde_json::from_value(tool_call.args.clone()) {
            Ok(req) => req,
            Err(e) => {
                return Ok(format!("❌ Invalid code_search arguments: {}", e));
            }
        };

    // Execute the code search
    match crate::code_search::execute_code_search(request).await {
        Ok(response) => {
            // Serialize the response to JSON
            match serde_json::to_string_pretty(&response) {
                Ok(json_output) => Ok(format!("✅ Code search completed\n{}", json_output)),
                Err(e) => Ok(format!("❌ Failed to serialize response: {}", e)),
            }
        }
        Err(e) => Ok(format!("❌ Code search failed: {}", e)),
    }
}

//! Tool dispatch module - routes tool calls to their implementations.
//!
//! This module provides a clean dispatch mechanism that routes tool calls
//! to the appropriate handler in the `tools/` module.

use anyhow::Result;
use tracing::{debug, warn};

use crate::tools::executor::ToolContext;
use crate::tools::{envelope, file_ops, memory, misc, plan, research, shell, toolsets, webdriver};
use crate::ui_writer::UiWriter;
use crate::ToolCall;

/// Dispatch a tool call to the appropriate handler.
///
/// This function routes tool calls to their implementations in the `tools/` module,
/// providing a single point of dispatch for all tool execution.
pub async fn dispatch_tool<W: UiWriter>(
    tool_call: &ToolCall,
    ctx: &mut ToolContext<'_, W>,
) -> Result<String> {
    debug!("Dispatching tool: {}", tool_call.tool);

    match tool_call.tool.as_str() {
        // Shell tools
        "shell" => shell::execute_shell(tool_call, ctx).await,
        "background_process" => shell::execute_background_process(tool_call, ctx).await,

        // File operations
        "read_file" => file_ops::execute_read_file(tool_call, ctx).await,
        "read_image" => file_ops::execute_read_image(tool_call, ctx).await,
        "write_file" => file_ops::execute_write_file(tool_call, ctx).await,
        "str_replace" => file_ops::execute_str_replace(tool_call, ctx).await,

        // Plan Mode
        "plan_read" => plan::execute_plan_read(tool_call, ctx).await,
        "plan_write" => plan::execute_plan_write(tool_call, ctx).await,
        "plan_approve" => plan::execute_plan_approve(tool_call, ctx).await,

        // Action Envelope
        "write_envelope" => envelope::execute_write_envelope(tool_call, ctx).await,

        // Miscellaneous tools
        "code_search" => misc::execute_code_search(tool_call, ctx).await,

        // Research tools
        "research" => research::execute_research(tool_call, ctx).await,
        "research_status" => research::execute_research_status(tool_call, ctx).await,
        
        // Workspace memory tools
        "remember" => memory::execute_remember(tool_call, ctx).await,

        // Toolset loading
        "load_toolset" => toolsets::execute_load_toolset(tool_call, ctx).await,


        // WebDriver tools
        "webdriver_start" => webdriver::execute_webdriver_start(tool_call, ctx).await,
        "webdriver_navigate" => webdriver::execute_webdriver_navigate(tool_call, ctx).await,
        "webdriver_get_url" => webdriver::execute_webdriver_get_url(tool_call, ctx).await,
        "webdriver_get_title" => webdriver::execute_webdriver_get_title(tool_call, ctx).await,
        "webdriver_find_element" => webdriver::execute_webdriver_find_element(tool_call, ctx).await,
        "webdriver_find_elements" => webdriver::execute_webdriver_find_elements(tool_call, ctx).await,
        "webdriver_click" => webdriver::execute_webdriver_click(tool_call, ctx).await,
        "webdriver_send_keys" => webdriver::execute_webdriver_send_keys(tool_call, ctx).await,
        "webdriver_execute_script" => webdriver::execute_webdriver_execute_script(tool_call, ctx).await,
        "webdriver_get_page_source" => webdriver::execute_webdriver_get_page_source(tool_call, ctx).await,
        "webdriver_screenshot" => webdriver::execute_webdriver_screenshot(tool_call, ctx).await,
        "webdriver_back" => webdriver::execute_webdriver_back(tool_call, ctx).await,
        "webdriver_forward" => webdriver::execute_webdriver_forward(tool_call, ctx).await,
        "webdriver_refresh" => webdriver::execute_webdriver_refresh(tool_call, ctx).await,
        "webdriver_quit" => webdriver::execute_webdriver_quit(tool_call, ctx).await,

        // Unknown tool
        _ => {
            warn!("Unknown tool: {}", tool_call.tool);
            Ok(format!("❓ Unknown tool: {}", tool_call.tool))
        }
    }
}

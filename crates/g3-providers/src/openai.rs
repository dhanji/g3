use anyhow::Result;
use async_trait::async_trait;
use bytes::Bytes;
use futures_util::stream::StreamExt;
use reqwest::Client;
use serde::Deserialize;
use serde_json::json;
use tokio::sync::mpsc;
use tokio_stream::wrappers::ReceiverStream;
use tracing::{debug, error};

use crate::{
    streaming::{make_final_chunk, make_text_chunk},
    CompletionChunk, CompletionRequest, CompletionResponse, CompletionStream, LLMProvider, Message,
    MessageRole, Tool, ToolCall, Usage,
};

#[derive(Clone)]
pub struct OpenAIProvider {
    client: Client,
    api_key: String,
    model: String,
    base_url: String,
    max_tokens: Option<u32>,
    _temperature: Option<f32>,
    reasoning_exclude: Option<bool>,
    name: String,
}

impl OpenAIProvider {
    pub fn new(
        api_key: String,
        model: Option<String>,
        base_url: Option<String>,
        max_tokens: Option<u32>,
        temperature: Option<f32>,
        reasoning_exclude: Option<bool>,
    ) -> Result<Self> {
        Self::new_with_name(
            "openai".to_string(),
            api_key,
            model,
            base_url,
            max_tokens,
            temperature,
            reasoning_exclude,
        )
    }

    pub fn new_with_name(
        name: String,
        api_key: String,
        model: Option<String>,
        base_url: Option<String>,
        max_tokens: Option<u32>,
        temperature: Option<f32>,
        reasoning_exclude: Option<bool>,
    ) -> Result<Self> {
        Ok(Self {
            client: Client::new(),
            api_key,
            model: model.unwrap_or_else(|| "gpt-4o".to_string()),
            base_url: base_url.unwrap_or_else(|| "https://api.openai.com/v1".to_string()),
            max_tokens,
            _temperature: temperature,
            reasoning_exclude,
            name,
        })
    }

    fn create_request_body(
        &self,
        messages: &[Message],
        tools: Option<&[Tool]>,
        stream: bool,
        max_tokens: Option<u32>,
        _temperature: Option<f32>,
    ) -> serde_json::Value {
        let mut body = json!({
            "model": self.model,
            "messages": convert_messages(messages),
            "stream": stream,
        });

        if let Some(max_tokens) = max_tokens.or(self.max_tokens) {
            body["max_completion_tokens"] = json!(max_tokens);
        }

        // OpenAI calls with temp setting seem to fail, so don't send one.
        // if let Some(temperature) = temperature.or(self.temperature) {
        //     body["temperature"] = json!(temperature);
        // }

        if let Some(tools) = tools {
            if !tools.is_empty() {
                body["tools"] = json!(convert_tools(tools));
            }
        }

        if stream {
            body["stream_options"] = json!({
                "include_usage": true,
            });
        }

        if self.reasoning_exclude == Some(true) {
            Self::set_reasoning_exclude(&mut body);
        }

        body
    }

    fn set_reasoning_exclude(body: &mut serde_json::Value) {
        let Some(body_obj) = body.as_object_mut() else {
            return;
        };

        match body_obj.get_mut("reasoning") {
            Some(reasoning) => {
                if let Some(reasoning_obj) = reasoning.as_object_mut() {
                    reasoning_obj
                        .entry("exclude".to_string())
                        .or_insert_with(|| json!(true));
                }
            }
            None => {
                body_obj.insert("reasoning".to_string(), json!({ "exclude": true }));
            }
        }
    }

    async fn parse_streaming_response(
        &self,
        mut stream: impl futures_util::Stream<Item = reqwest::Result<Bytes>> + Unpin,
        tx: mpsc::Sender<Result<CompletionChunk>>,
    ) -> Option<Usage> {
        let mut buffer = String::new();
        let mut accumulated_content = String::new();
        let mut accumulated_usage: Option<Usage> = None;
        let mut current_tool_calls: Vec<OpenAIStreamingToolCall> = Vec::new();

        while let Some(chunk_result) = stream.next().await {
            match chunk_result {
                Ok(chunk) => {
                    let chunk_str = match std::str::from_utf8(&chunk) {
                        Ok(s) => s,
                        Err(e) => {
                            error!("Failed to parse chunk as UTF-8: {}", e);
                            continue;
                        }
                    };

                    buffer.push_str(chunk_str);

                    // Process complete lines
                    while let Some(line_end) = buffer.find('\n') {
                        let line = buffer[..line_end].trim().to_string();
                        buffer.drain(..line_end + 1);

                        if line.is_empty() {
                            continue;
                        }

                        // Parse Server-Sent Events format
                        if let Some(data) = line.strip_prefix("data: ") {
                            if data == "[DONE]" {
                                debug!("Received stream completion marker");

                                // Send final chunk with accumulated content and tool calls
                                if !accumulated_content.is_empty() || !current_tool_calls.is_empty()
                                {
                                    let tool_calls = if current_tool_calls.is_empty() {
                                        None
                                    } else {
                                        Some(
                                            current_tool_calls
                                                .iter()
                                                .filter_map(|tc| tc.to_tool_call())
                                                .collect(),
                                        )
                                    };

                                    let final_chunk = CompletionChunk {
                                        content: accumulated_content.clone(),
                                        finished: true,
                                        tool_calls,
                                        usage: accumulated_usage.clone(),
                                        stop_reason: None, // TODO: Extract from OpenAI response
                                        tool_call_streaming: None,
                                    };
                                    let _ = tx.send(Ok(final_chunk)).await;
                                }

                                return accumulated_usage;
                            }

                            // Parse the JSON data
                            match serde_json::from_str::<OpenAIStreamChunk>(data) {
                                Ok(chunk_data) => {
                                    // Handle content
                                    for choice in &chunk_data.choices {
                                        if let Some(content) = &choice.delta.content {
                                            accumulated_content.push_str(content);

                                            let chunk = make_text_chunk(content.clone());
                                            if tx.send(Ok(chunk)).await.is_err() {
                                                debug!("Receiver dropped, stopping stream");
                                                return accumulated_usage;
                                            }
                                        }

                                        // Handle tool calls
                                        if let Some(delta_tool_calls) = &choice.delta.tool_calls {
                                            for delta_tool_call in delta_tool_calls {
                                                if let Some(index) = delta_tool_call.index {
                                                    // Ensure we have enough tool calls in our vector
                                                    while current_tool_calls.len() <= index {
                                                        current_tool_calls.push(
                                                            OpenAIStreamingToolCall::default(),
                                                        );
                                                    }

                                                    let tool_call = &mut current_tool_calls[index];

                                                    if let Some(id) = &delta_tool_call.id {
                                                        tool_call.id = Some(id.clone());
                                                    }

                                                    if let Some(function) =
                                                        &delta_tool_call.function
                                                    {
                                                        if let Some(name) = &function.name {
                                                            tool_call.name = Some(name.clone());
                                                        }
                                                        if let Some(arguments) = &function.arguments
                                                        {
                                                            tool_call.arguments.push_str(arguments);
                                                        }
                                                    }
                                                }
                                            }
                                        }
                                    }

                                    // Handle usage
                                    if let Some(usage) = chunk_data.usage {
                                        accumulated_usage = Some(Usage {
                                            prompt_tokens: usage.prompt_tokens,
                                            completion_tokens: usage.completion_tokens,
                                            total_tokens: usage.total_tokens,
                                            cache_creation_tokens: 0, // OpenAI doesn't report cache creation
                                            cache_read_tokens: usage
                                                .prompt_tokens_details
                                                .as_ref()
                                                .map(|d| d.cached_tokens)
                                                .unwrap_or(0),
                                        });
                                    }
                                }
                                Err(e) => {
                                    debug!("Failed to parse stream chunk: {} - Data: {}", e, data);
                                }
                            }
                        }
                    }
                }
                Err(e) => {
                    error!("Stream error: {}", e);
                    let _ = tx.send(Err(anyhow::anyhow!("Stream error: {}", e))).await;
                    return accumulated_usage;
                }
            }
        }

        // Send final chunk if we haven't already
        let tool_calls = if current_tool_calls.is_empty() {
            vec![]
        } else {
            current_tool_calls
                .iter()
                .filter_map(|tc| tc.to_tool_call())
                .collect()
        };

        let final_chunk = make_final_chunk(tool_calls, accumulated_usage.clone());
        let _ = tx.send(Ok(final_chunk)).await;

        accumulated_usage
    }
}

#[async_trait]
impl LLMProvider for OpenAIProvider {
    async fn complete(&self, request: CompletionRequest) -> Result<CompletionResponse> {
        debug!(
            "Processing OpenAI completion request with {} messages",
            request.messages.len()
        );

        let body = self.create_request_body(
            &request.messages,
            request.tools.as_deref(),
            false,
            request.max_tokens,
            request.temperature,
        );

        debug!("Sending request to OpenAI API: model={}", self.model);

        let response = self
            .client
            .post(format!("{}/chat/completions", self.base_url))
            .header("Authorization", format!("Bearer {}", self.api_key))
            .json(&body)
            .send()
            .await?;

        let status = response.status();
        if !status.is_success() {
            let error_text = response
                .text()
                .await
                .unwrap_or_else(|_| "Unknown error".to_string());
            return Err(anyhow::anyhow!(
                "OpenAI API error {}: {}",
                status,
                error_text
            ));
        }

        let openai_response: OpenAIResponse = response.json().await?;

        let content = openai_response
            .choices
            .first()
            .and_then(|choice| choice.message.content.clone())
            .unwrap_or_default();

        let usage = Usage {
            prompt_tokens: openai_response.usage.prompt_tokens,
            completion_tokens: openai_response.usage.completion_tokens,
            total_tokens: openai_response.usage.total_tokens,
            cache_creation_tokens: 0, // OpenAI doesn't report cache creation
            cache_read_tokens: openai_response
                .usage
                .prompt_tokens_details
                .as_ref()
                .map(|d| d.cached_tokens)
                .unwrap_or(0),
        };

        debug!(
            "OpenAI completion successful: {} tokens generated",
            usage.completion_tokens
        );

        Ok(CompletionResponse {
            content,
            usage,
            model: self.model.clone(),
        })
    }

    async fn stream(&self, request: CompletionRequest) -> Result<CompletionStream> {
        debug!(
            "Processing OpenAI streaming request with {} messages",
            request.messages.len()
        );

        let body = self.create_request_body(
            &request.messages,
            request.tools.as_deref(),
            true,
            request.max_tokens,
            request.temperature,
        );

        debug!(
            "Sending streaming request to OpenAI API: model={}",
            self.model
        );

        let response = self
            .client
            .post(format!("{}/chat/completions", self.base_url))
            .header("Authorization", format!("Bearer {}", self.api_key))
            .json(&body)
            .send()
            .await?;

        let status = response.status();
        if !status.is_success() {
            let error_text = response
                .text()
                .await
                .unwrap_or_else(|_| "Unknown error".to_string());
            return Err(anyhow::anyhow!(
                "OpenAI API error {}: {}",
                status,
                error_text
            ));
        }

        let stream = response.bytes_stream();
        let (tx, rx) = mpsc::channel(100);

        // Spawn task to process the stream
        let provider = self.clone();
        tokio::spawn(async move {
            let usage = provider.parse_streaming_response(stream, tx).await;
            // Log the final usage if available
            if let Some(usage) = usage {
                debug!(
                    "Stream completed with usage - prompt: {}, completion: {}, total: {}",
                    usage.prompt_tokens, usage.completion_tokens, usage.total_tokens
                );
            }
        });

        Ok(ReceiverStream::new(rx))
    }

    fn name(&self) -> &str {
        &self.name
    }

    fn model(&self) -> &str {
        &self.model
    }

    fn has_native_tool_calling(&self) -> bool {
        // OpenAI models support native tool calling
        true
    }

    fn max_tokens(&self) -> u32 {
        self.max_tokens.unwrap_or(32000)
    }

    fn temperature(&self) -> f32 {
        self._temperature.unwrap_or(0.1)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn make_provider(reasoning_exclude: Option<bool>) -> OpenAIProvider {
        OpenAIProvider::new_with_name(
            "openai.test".to_string(),
            "key".to_string(),
            Some("gpt-4o-mini".to_string()),
            None,
            None,
            None,
            reasoning_exclude,
        )
        .expect("provider should construct")
    }

    #[test]
    fn create_request_body_includes_reasoning_exclude_when_enabled() {
        let provider = make_provider(Some(true));
        let messages = vec![Message::new(MessageRole::User, "ping".to_string())];
        let body = provider.create_request_body(&messages, None, false, None, None);

        assert_eq!(body["reasoning"]["exclude"], json!(true));
    }

    #[test]
    fn create_request_body_skips_reasoning_exclude_when_disabled() {
        let provider = make_provider(None);
        let messages = vec![Message::new(MessageRole::User, "ping".to_string())];
        let body = provider.create_request_body(&messages, None, false, None, None);

        assert!(body.get("reasoning").is_none());
    }

    #[test]
    fn set_reasoning_exclude_merges_without_overwriting_existing_exclude() {
        let mut body = json!({
            "model": "gpt-4o-mini",
            "reasoning": {
                "effort": "medium",
                "exclude": false
            }
        });

        OpenAIProvider::set_reasoning_exclude(&mut body);

        assert_eq!(body["reasoning"]["effort"], json!("medium"));
        assert_eq!(body["reasoning"]["exclude"], json!(false));
    }

    #[test]
    fn set_reasoning_exclude_adds_exclude_when_reasoning_present_without_it() {
        let mut body = json!({
            "model": "gpt-4o-mini",
            "reasoning": {
                "effort": "high"
            }
        });

        OpenAIProvider::set_reasoning_exclude(&mut body);

        assert_eq!(body["reasoning"]["effort"], json!("high"));
        assert_eq!(body["reasoning"]["exclude"], json!(true));
    }
}

fn convert_messages(messages: &[Message]) -> Vec<serde_json::Value> {
    messages
        .iter()
        .map(|msg| {
            json!({
                "role": match msg.role {
                    MessageRole::System => "system",
                    MessageRole::User => "user",
                    MessageRole::Assistant => "assistant",
                },
                "content": msg.content,
            })
        })
        .collect()
}

fn convert_tools(tools: &[Tool]) -> Vec<serde_json::Value> {
    tools
        .iter()
        .map(|tool| {
            json!({
                "type": "function",
                "function": {
                    "name": tool.name,
                    "description": tool.description,
                    "parameters": tool.input_schema,
                }
            })
        })
        .collect()
}

// OpenAI API response structures
#[derive(Debug, Deserialize)]
struct OpenAIResponse {
    choices: Vec<OpenAIChoice>,
    usage: OpenAIUsage,
}

#[derive(Debug, Deserialize)]
struct OpenAIChoice {
    message: OpenAIMessage,
}

#[allow(dead_code)]
#[derive(Debug, Deserialize)]
struct OpenAIMessage {
    content: Option<String>,
    #[serde(default)]
    tool_calls: Option<Vec<OpenAIToolCall>>,
}

#[allow(dead_code)]
#[derive(Debug, Deserialize)]
struct OpenAIToolCall {
    id: String,
    function: OpenAIFunction,
}

#[allow(dead_code)]
#[derive(Debug, Deserialize)]
struct OpenAIFunction {
    name: String,
    arguments: String,
}

// Streaming tool call accumulator
#[derive(Debug, Default)]
struct OpenAIStreamingToolCall {
    id: Option<String>,
    name: Option<String>,
    arguments: String,
}

impl OpenAIStreamingToolCall {
    fn to_tool_call(&self) -> Option<ToolCall> {
        let id = self.id.as_ref()?;
        let name = self.name.as_ref()?;

        let args = serde_json::from_str(&self.arguments).unwrap_or(serde_json::Value::Null);

        Some(ToolCall {
            id: id.clone(),
            tool: name.clone(),
            args,
        })
    }
}

#[derive(Debug, Deserialize)]
struct OpenAIUsage {
    prompt_tokens: u32,
    completion_tokens: u32,
    total_tokens: u32,
    /// Detailed breakdown of prompt tokens including cache info
    #[serde(default)]
    prompt_tokens_details: Option<OpenAIPromptTokensDetails>,
}

#[derive(Debug, Deserialize, Default)]
struct OpenAIPromptTokensDetails {
    /// Tokens retrieved from cache (cache hit)
    #[serde(default)]
    cached_tokens: u32,
}

// Streaming response structures
#[derive(Debug, Deserialize)]
struct OpenAIStreamChunk {
    choices: Vec<OpenAIStreamChoice>,
    usage: Option<OpenAIUsage>,
}

#[derive(Debug, Deserialize)]
struct OpenAIStreamChoice {
    delta: OpenAIDelta,
}

#[derive(Debug, Deserialize)]
struct OpenAIDelta {
    content: Option<String>,
    #[serde(default)]
    tool_calls: Option<Vec<OpenAIDeltaToolCall>>,
}

#[derive(Debug, Deserialize)]
struct OpenAIDeltaToolCall {
    index: Option<usize>,
    id: Option<String>,
    function: Option<OpenAIDeltaFunction>,
}

#[derive(Debug, Deserialize)]
struct OpenAIDeltaFunction {
    name: Option<String>,
    arguments: Option<String>,
}

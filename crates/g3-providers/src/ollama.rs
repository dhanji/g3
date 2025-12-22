//! Ollama provider using native Ollama API
//!
//! This provider uses Ollama's native /api/chat endpoint instead of the OpenAI-compatible API,
//! providing access to Ollama-specific features like context window configuration and model introspection.

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
    CompletionChunk, CompletionRequest, CompletionResponse, CompletionStream, LLMProvider, Message,
    MessageRole, Tool, ToolCall, Usage,
};

/// Ollama provider configuration options
#[derive(Debug, Clone)]
pub struct OllamaOptions {
    pub num_ctx: Option<u32>,
    pub num_gpu: Option<i32>,
    pub keep_alive: Option<String>,
    pub temperature: Option<f32>,
    pub top_p: Option<f32>,
    pub top_k: Option<i32>,
    pub repeat_penalty: Option<f32>,
    pub seed: Option<i64>,
}

impl Default for OllamaOptions {
    fn default() -> Self {
        Self {
            num_ctx: None,
            num_gpu: None,
            keep_alive: None,
            temperature: Some(0.1),
            top_p: None,
            top_k: None,
            repeat_penalty: None,
            seed: None,
        }
    }
}

#[derive(Clone)]
pub struct OllamaProvider {
    client: Client,
    name: String,
    base_url: String,
    model: String,
    max_tokens: Option<u32>,
    options: OllamaOptions,
    /// Cached context size from model metadata
    detected_num_ctx: Option<u32>,
}

impl OllamaProvider {
    pub fn new(
        base_url: String,
        model: String,
        max_tokens: Option<u32>,
        options: OllamaOptions,
    ) -> Self {
        Self::new_with_name("ollama".to_string(), base_url, model, max_tokens, options)
    }

    pub fn new_with_name(
        name: String,
        base_url: String,
        model: String,
        max_tokens: Option<u32>,
        options: OllamaOptions,
    ) -> Self {
        Self {
            client: Client::new(),
            name,
            base_url: base_url.trim_end_matches('/').to_string(),
            model,
            max_tokens,
            options,
            detected_num_ctx: None,
        }
    }

    /// Query Ollama for model metadata and return the context size
    pub async fn detect_context_size(&self) -> Result<u32> {
        let response = self
            .client
            .post(format!("{}/api/show", self.base_url))
            .json(&json!({ "name": self.model }))
            .send()
            .await?;

        if !response.status().is_success() {
            let error_text = response.text().await.unwrap_or_default();
            return Err(anyhow::anyhow!(
                "Failed to get model info: {}",
                error_text
            ));
        }

        let model_info: OllamaModelInfo = response.json().await?;

        // Try to get num_ctx from model parameters
        if let Some(params) = model_info.parameters {
            // Parameters come as a string with key-value pairs
            for line in params.lines() {
                if line.starts_with("num_ctx") {
                    if let Some(value) = line.split_whitespace().last() {
                        if let Ok(ctx) = value.parse::<u32>() {
                            return Ok(ctx);
                        }
                    }
                }
            }
        }

        // Default context size if not found
        Ok(4096)
    }

    /// Get the effective context size (configured or detected)
    pub fn effective_num_ctx(&self) -> Option<u32> {
        self.options.num_ctx.or(self.detected_num_ctx)
    }

    fn build_options(&self) -> serde_json::Value {
        let mut options = json!({});

        if let Some(num_ctx) = self.effective_num_ctx() {
            options["num_ctx"] = json!(num_ctx);
        }
        if let Some(num_gpu) = self.options.num_gpu {
            options["num_gpu"] = json!(num_gpu);
        }
        if let Some(temp) = self.options.temperature {
            options["temperature"] = json!(temp);
        }
        if let Some(top_p) = self.options.top_p {
            options["top_p"] = json!(top_p);
        }
        if let Some(top_k) = self.options.top_k {
            options["top_k"] = json!(top_k);
        }
        if let Some(repeat_penalty) = self.options.repeat_penalty {
            options["repeat_penalty"] = json!(repeat_penalty);
        }
        if let Some(seed) = self.options.seed {
            if seed >= 0 {
                options["seed"] = json!(seed);
            }
        }
        if let Some(max_tokens) = self.max_tokens {
            options["num_predict"] = json!(max_tokens);
        }

        options
    }

    fn create_request_body(
        &self,
        messages: &[Message],
        tools: Option<&[Tool]>,
        stream: bool,
    ) -> serde_json::Value {
        let mut body = json!({
            "model": self.model,
            "messages": convert_messages(messages),
            "stream": stream,
            "options": self.build_options(),
        });

        if let Some(keep_alive) = &self.options.keep_alive {
            body["keep_alive"] = json!(keep_alive);
        }

        // Ollama supports tools in newer versions
        if let Some(tools) = tools {
            if !tools.is_empty() {
                body["tools"] = json!(convert_tools(tools));
            }
        }

        body
    }

    async fn parse_streaming_response(
        &self,
        mut stream: impl futures_util::Stream<Item = reqwest::Result<Bytes>> + Unpin,
        tx: mpsc::Sender<Result<CompletionChunk>>,
    ) -> Option<Usage> {
        let mut buffer = String::new();
        let mut accumulated_content = String::new();
        let mut accumulated_usage: Option<Usage> = None;
        let mut current_tool_calls: Vec<OllamaStreamingToolCall> = Vec::new();

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

                    // Ollama sends newline-delimited JSON
                    while let Some(line_end) = buffer.find('\n') {
                        let line = buffer[..line_end].trim().to_string();
                        buffer.drain(..line_end + 1);

                        if line.is_empty() {
                            continue;
                        }

                        match serde_json::from_str::<OllamaStreamChunk>(&line) {
                            Ok(chunk_data) => {
                                // Handle message content
                                if let Some(ref message) = chunk_data.message {
                                    if let Some(ref content) = message.content {
                                        if !content.is_empty() {
                                            accumulated_content.push_str(content);

                                            let chunk = CompletionChunk {
                                                content: content.clone(),
                                                finished: false,
                                                tool_calls: None,
                                                usage: None,
                                            };
                                            if tx.send(Ok(chunk)).await.is_err() {
                                                debug!("Receiver dropped, stopping stream");
                                                return accumulated_usage;
                                            }
                                        }
                                    }

                                    // Handle tool calls
                                    if let Some(ref tool_calls) = message.tool_calls {
                                        for tc in tool_calls {
                                            current_tool_calls.push(OllamaStreamingToolCall {
                                                name: tc.function.name.clone(),
                                                arguments: tc.function.arguments.clone(),
                                            });
                                        }
                                    }
                                }

                                // Check if this is the final chunk
                                if chunk_data.done {
                                    // Calculate usage from Ollama's metrics
                                    let prompt_tokens = chunk_data.prompt_eval_count.unwrap_or(0);
                                    let completion_tokens = chunk_data.eval_count.unwrap_or(0);

                                    accumulated_usage = Some(Usage {
                                        prompt_tokens,
                                        completion_tokens,
                                        total_tokens: prompt_tokens + completion_tokens,
                                    });

                                    // Send final chunk
                                    let tool_calls = if current_tool_calls.is_empty() {
                                        None
                                    } else {
                                        Some(
                                            current_tool_calls
                                                .iter()
                                                .enumerate()
                                                .map(|(i, tc)| tc.to_tool_call(i))
                                                .collect(),
                                        )
                                    };

                                    let final_chunk = CompletionChunk {
                                        content: String::new(),
                                        finished: true,
                                        tool_calls,
                                        usage: accumulated_usage.clone(),
                                    };
                                    let _ = tx.send(Ok(final_chunk)).await;

                                    return accumulated_usage;
                                }
                            }
                            Err(e) => {
                                debug!("Failed to parse Ollama stream chunk: {} - Line: {}", e, line);
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

        // Send final chunk if stream ended without done marker
        let final_chunk = CompletionChunk {
            content: String::new(),
            finished: true,
            tool_calls: None,
            usage: accumulated_usage.clone(),
        };
        let _ = tx.send(Ok(final_chunk)).await;

        accumulated_usage
    }
}

#[async_trait]
impl LLMProvider for OllamaProvider {
    async fn complete(&self, request: CompletionRequest) -> Result<CompletionResponse> {
        debug!(
            "Processing Ollama completion request with {} messages",
            request.messages.len()
        );

        let body = self.create_request_body(
            &request.messages,
            request.tools.as_deref(),
            false,
        );

        debug!("Sending request to Ollama API: model={}", self.model);

        let response = self
            .client
            .post(format!("{}/api/chat", self.base_url))
            .json(&body)
            .send()
            .await?;

        let status = response.status();
        if !status.is_success() {
            let error_text = response.text().await.unwrap_or_default();

            // Check for common Ollama errors
            if error_text.contains("model") && error_text.contains("not found") {
                return Err(anyhow::anyhow!(
                    "Model '{}' not found. Run: ollama pull {}",
                    self.model,
                    self.model
                ));
            }

            return Err(anyhow::anyhow!(
                "Ollama API error {}: {}",
                status,
                error_text
            ));
        }

        let ollama_response: OllamaResponse = response.json().await?;

        let content = ollama_response
            .message
            .content
            .unwrap_or_default();

        // Convert Ollama metrics to usage
        let prompt_tokens = ollama_response.prompt_eval_count.unwrap_or(0);
        let completion_tokens = ollama_response.eval_count.unwrap_or(0);

        let usage = Usage {
            prompt_tokens,
            completion_tokens,
            total_tokens: prompt_tokens + completion_tokens,
        };

        debug!(
            "Ollama completion successful: {} tokens generated",
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
            "Processing Ollama streaming request with {} messages",
            request.messages.len()
        );

        let body = self.create_request_body(
            &request.messages,
            request.tools.as_deref(),
            true,
        );

        debug!(
            "Sending streaming request to Ollama API: model={}",
            self.model
        );

        let response = self
            .client
            .post(format!("{}/api/chat", self.base_url))
            .json(&body)
            .send()
            .await?;

        let status = response.status();
        if !status.is_success() {
            let error_text = response.text().await.unwrap_or_default();

            if error_text.contains("model") && error_text.contains("not found") {
                return Err(anyhow::anyhow!(
                    "Model '{}' not found. Run: ollama pull {}",
                    self.model,
                    self.model
                ));
            }

            return Err(anyhow::anyhow!(
                "Ollama API error {}: {}",
                status,
                error_text
            ));
        }

        let stream = response.bytes_stream();
        let (tx, rx) = mpsc::channel(100);

        let provider = self.clone();
        tokio::spawn(async move {
            let usage = provider.parse_streaming_response(stream, tx).await;
            if let Some(usage) = usage {
                debug!(
                    "Ollama stream completed - prompt: {}, completion: {}, total: {}",
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
        // Ollama supports tool calling in newer versions
        true
    }

    fn max_tokens(&self) -> u32 {
        self.max_tokens.unwrap_or(4096)
    }

    fn temperature(&self) -> f32 {
        self.options.temperature.unwrap_or(0.1)
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

// Ollama API response structures

#[derive(Debug, Deserialize)]
struct OllamaModelInfo {
    #[allow(dead_code)]
    modelfile: Option<String>,
    parameters: Option<String>,
    #[allow(dead_code)]
    template: Option<String>,
}

#[derive(Debug, Deserialize)]
struct OllamaResponse {
    message: OllamaMessage,
    #[allow(dead_code)]
    done: bool,
    #[allow(dead_code)]
    total_duration: Option<u64>,
    #[allow(dead_code)]
    load_duration: Option<u64>,
    prompt_eval_count: Option<u32>,
    #[allow(dead_code)]
    prompt_eval_duration: Option<u64>,
    eval_count: Option<u32>,
    #[allow(dead_code)]
    eval_duration: Option<u64>,
}

#[derive(Debug, Deserialize)]
struct OllamaMessage {
    #[allow(dead_code)]
    role: String,
    content: Option<String>,
    tool_calls: Option<Vec<OllamaToolCall>>,
}

#[derive(Debug, Deserialize)]
struct OllamaToolCall {
    function: OllamaFunction,
}

#[derive(Debug, Deserialize)]
struct OllamaFunction {
    name: String,
    arguments: serde_json::Value,
}

// Streaming response structures

#[derive(Debug, Deserialize)]
struct OllamaStreamChunk {
    #[allow(dead_code)]
    model: Option<String>,
    message: Option<OllamaMessage>,
    done: bool,
    prompt_eval_count: Option<u32>,
    eval_count: Option<u32>,
}

#[derive(Debug)]
struct OllamaStreamingToolCall {
    name: String,
    arguments: serde_json::Value,
}

impl OllamaStreamingToolCall {
    fn to_tool_call(&self, index: usize) -> ToolCall {
        ToolCall {
            id: format!("call_{}", index),
            tool: self.name.clone(),
            args: self.arguments.clone(),
        }
    }
}

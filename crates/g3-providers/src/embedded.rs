use anyhow::Result;
use crate::{
    CompletionChunk, CompletionRequest, CompletionResponse, CompletionStream, LLMProvider, Message,
    MessageRole, Usage,
};
use llama_cpp::{
    standard_sampler::{SamplerStage, StandardSampler},
    LlamaModel, LlamaParams, LlamaSession, SessionParams,
};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use tokio::sync::mpsc;
use tokio::sync::Mutex;
use tokio_stream::wrappers::ReceiverStream;
use tracing::{debug, error, info};

pub struct EmbeddedProvider {
    session: Arc<Mutex<LlamaSession>>,
    model_name: String,
    max_tokens: u32,
    temperature: f32,
    context_length: u32,
}

impl EmbeddedProvider {
    pub fn new(
        model_path: String,
        model_type: String,
        context_length: Option<u32>,
        max_tokens: Option<u32>,
        temperature: Option<f32>,
        gpu_layers: Option<u32>,
        threads: Option<u32>,
    ) -> Result<Self> {
        info!("Loading embedded model from: {}", model_path);

        // Expand tilde in path
        let expanded_path = shellexpand::tilde(&model_path);
        let model_path_buf = PathBuf::from(expanded_path.as_ref());
        
        // If model doesn't exist and it's the default Qwen model, offer to download it
        if !model_path_buf.exists() {
            if model_path.contains("qwen2.5-7b-instruct-q3_k_m.gguf") {
                info!("Model file not found. Attempting to download Qwen 2.5 7B model...");
                Self::download_qwen_model(&model_path_buf)?;
            } else {
                anyhow::bail!("Model file not found: {}", model_path_buf.display());
            }
        }
        
        let model_path = model_path_buf.as_path();

        // Set up model parameters
        let mut params = LlamaParams::default();

        if let Some(gpu_layers) = gpu_layers {
            params.n_gpu_layers = gpu_layers;
            info!("Using {} GPU layers", gpu_layers);
        }

        let context_size = context_length.unwrap_or(4096);
        info!("Using context length: {}", context_size);

        // Load the model
        info!("Loading model...");
        let model = LlamaModel::load_from_file(model_path, params)
            .map_err(|e| anyhow::anyhow!("Failed to load model: {}", e))?;

        // Create session with parameters
        let mut session_params = SessionParams {
            n_ctx: context_size,
            ..Default::default()
        };
        if let Some(threads) = threads {
            session_params.n_threads = threads;
        }

        let session = model
            .create_session(session_params)
            .map_err(|e| anyhow::anyhow!("Failed to create session: {}", e))?;

        info!("Successfully loaded {} model", model_type);

        Ok(Self {
            session: Arc::new(Mutex::new(session)),
            model_name: format!("embedded-{}", model_type),
            max_tokens: max_tokens.unwrap_or(2048),
            temperature: temperature.unwrap_or(0.1),
            context_length: context_size,
        })
    }

    fn format_messages(&self, messages: &[Message]) -> String {
        // Determine the appropriate format based on model type
        let model_name_lower = self.model_name.to_lowercase();
        
        if model_name_lower.contains("qwen") {
            // Qwen format: <|im_start|>role\ncontent<|im_end|>
            let mut formatted = String::new();
            
            for message in messages {
                let role = match message.role {
                    MessageRole::System => "system",
                    MessageRole::User => "user", 
                    MessageRole::Assistant => "assistant",
                };
                
                formatted.push_str(&format!(
                    "<|im_start|>{}\n{}<|im_end|>\n",
                    role, message.content
                ));
            }
            
            // Add the start of assistant response
            formatted.push_str("<|im_start|>assistant\n");
            formatted
        } else if model_name_lower.contains("mistral") {
            // Mistral Instruct format: <s>[INST] ... [/INST] assistant_response</s>
            let mut formatted = String::new();
            let mut in_conversation = false;
            
            for (i, message) in messages.iter().enumerate() {
                match message.role {
                    MessageRole::System => {
                        // Mistral doesn't have a special system token, include it at the start
                        if i == 0 {
                            formatted.push_str("<s>[INST] ");
                            formatted.push_str(&message.content);
                            formatted.push_str("\n\n");
                            in_conversation = true;
                        }
                    }
                    MessageRole::User => {
                        if !in_conversation {
                            formatted.push_str("<s>[INST] ");
                        }
                        formatted.push_str(&message.content);
                        formatted.push_str(" [/INST]");
                        in_conversation = false;
                    }
                    MessageRole::Assistant => {
                        formatted.push(' ');
                        formatted.push_str(&message.content);
                        formatted.push_str("</s> ");
                        in_conversation = false;
                    }
                }
            }
            
            // If the last message was from user, add a space for the assistant's response
            if messages.last().is_some_and(|m| matches!(m.role, MessageRole::User)) {
                formatted.push(' ');
            }
            
            formatted
        } else {
            // Use Llama/CodeLlama format for other models
            let mut formatted = String::new();

            for message in messages {
                match message.role {
                    MessageRole::System => {
                        formatted.push_str(&format!(
                            "[INST] <<SYS>>\n{}\n<</SYS>>\n\n",
                            message.content
                        ));
                    }
                    MessageRole::User => {
                        formatted.push_str(&format!("{} [/INST] ", message.content));
                    }
                    MessageRole::Assistant => {
                        formatted.push_str(&format!("{} </s><s>[INST] ", message.content));
                    }
                }
            }
            formatted
        }
    }

    async fn generate_completion(
        &self,
        prompt: &str,
        max_tokens: u32,
        temperature: f32,
    ) -> Result<String> {
        let session = self.session.clone();
        let prompt = prompt.to_string();

        // Calculate dynamic max tokens based on available context headroom
        let prompt_tokens = self.estimate_tokens(&prompt);
        let available_tokens = self
            .context_length
            .saturating_sub(prompt_tokens)
            .saturating_sub(50); // Reserve 50 tokens for safety
        let dynamic_max_tokens = std::cmp::min(max_tokens as usize, available_tokens as usize);

        debug!("Context calculation: prompt_tokens={}, context_length={}, available_tokens={}, dynamic_max_tokens={}",
               prompt_tokens, self.context_length, available_tokens, dynamic_max_tokens);

        // Get stop sequences before entering the closure
        let stop_sequences = self.get_stop_sequences();

        // Add timeout to the entire operation
        let timeout_duration = std::time::Duration::from_secs(30); // Increased timeout for larger contexts

        let result = tokio::time::timeout(
            timeout_duration,
            tokio::task::spawn_blocking(move || {
                // Retry logic for acquiring the session lock
                let mut session_guard = None;
                for attempt in 0..5 {
                    match session.try_lock() {
                        Ok(ctx) => {
                            session_guard = Some(ctx);
                            break;
                        }
                        Err(_) => {
                            if attempt < 4 {
                                debug!("Session busy, retrying in {}ms (attempt {}/5)", 100 * (attempt + 1), attempt + 1);
                                std::thread::sleep(std::time::Duration::from_millis(100 * (attempt + 1) as u64));
                            } else {
                                return Err(anyhow::anyhow!("Model is busy after 5 attempts, please try again"));
                            }
                        }
                    }
                }
                
                let mut session = session_guard.ok_or_else(|| anyhow::anyhow!("Failed to acquire session lock"))?;

                debug!(
                    "Starting inference with prompt length: {} chars, estimated {} tokens",
                    prompt.len(),
                    prompt_tokens
                );

                // Set context to the prompt
                debug!("About to call set_context...");
                session
                    .set_context(&prompt)
                    .map_err(|e| anyhow::anyhow!("Failed to set context: {}", e))?;
                debug!("set_context completed successfully");

                // Create sampler with temperature
                debug!("Creating sampler...");
                let stages = vec![
                    SamplerStage::Temperature(temperature),
                    SamplerStage::TopK(40),
                    SamplerStage::TopP(0.9),
                ];
                let sampler = StandardSampler::new_softmax(stages, 1);
                debug!("Sampler created successfully");

                // Start completion with dynamic max tokens
                debug!(
                    "About to call start_completing_with with {} max tokens...",
                    dynamic_max_tokens
                );
                let mut completion_handle = session
                    .start_completing_with(sampler, dynamic_max_tokens)
                    .map_err(|e| anyhow::anyhow!("Failed to start completion: {}", e))?;
                debug!("start_completing_with completed successfully");

                let mut generated_text = String::new();
                let mut token_count = 0;
                let start_time = std::time::Instant::now();

                debug!("Starting token generation loop...");

                // Generate tokens with dynamic limits
                while let Some(token) = completion_handle.next_token() {
                    // Check for timeout on each token
                    if start_time.elapsed() > std::time::Duration::from_secs(25) {
                        debug!("Token generation timeout after {} tokens", token_count);
                        break;
                    }

                    let token_string = session.model().token_to_piece(token);
                    generated_text.push_str(&token_string);
                    token_count += 1;

                    if token_count <= 10 || token_count % 50 == 0 {
                        debug!("Generated token {}: '{}'", token_count, token_string);
                    }

                    // Use dynamic token limit
                    if token_count >= dynamic_max_tokens {
                        debug!("Reached dynamic token limit: {}", dynamic_max_tokens);
                        break;
                    }

                    // Stop on completion markers
                    let mut hit_stop = false;
                    for stop_seq in &stop_sequences {
                        if generated_text.contains(stop_seq) {
                            debug!("Hit stop sequence '{}' at {} tokens", stop_seq, token_count);
                            hit_stop = true;
                            break;
                        }
                    }
                    
                    if hit_stop {
                        break;
                    }
                }

                debug!(
                    "Token generation loop completed. Generated {} tokens in {:?}",
                    token_count,
                    start_time.elapsed()
                );
                
                Ok((generated_text, token_count))
            }),
        )
        .await;

        match result {
            Ok(inner_result) => match inner_result {
                Ok(task_result) => match task_result {
                    Ok((text, token_count)) => {
                        info!(
                            "Completed generation: {} tokens (dynamic limit was {})",
                            token_count, dynamic_max_tokens
                        );
                        // Clean stop sequences from the generated text after the closure
                        Ok(self.clean_stop_sequences(&text))
                    }
                    Err(e) => Err(e),
                },
                Err(e) => Err(e.into()),
            },
            Err(_) => {
                error!("Generation timed out after 30 seconds");
                Err(anyhow::anyhow!("Generation timed out"))
            }
        }
    }

    // Helper function to estimate token count from text
    fn estimate_tokens(&self, text: &str) -> u32 {
        // Rough estimation: average 4 characters per token
        // This is conservative - actual tokenization might be different
        (text.len() as f32 / 4.0).ceil() as u32
    }

    // Helper function to get stop sequences based on model type
    fn get_stop_sequences(&self) -> Vec<&'static str> {
        // Determine model type from model_name
        let model_name_lower = self.model_name.to_lowercase();
        
        if model_name_lower.contains("qwen") {
            vec![
                "<|im_end|>",     // Qwen ChatML format end token
                "<|endoftext|>",  // Alternative end token
                "</s>",           // Generic end of sequence
                "<|im_start|>",   // Start of new message (shouldn't appear in response)
            ]
        } else if model_name_lower.contains("codellama") || model_name_lower.contains("code-llama") {
            vec![
                "</s>",           // End of sequence
                "[/INST]",        // End of instruction  
                "<</SYS>>",       // End of system message
                "[INST]",         // Start of new instruction (shouldn't appear in response)
                "<<SYS>>",        // Start of system (shouldn't appear in response)
            ]
        } else if model_name_lower.contains("llama") {
            vec![
                "</s>",           // End of sequence
                "[/INST]",        // End of instruction
                "<</SYS>>",       // End of system message
                "### Human:",     // Conversation format
                "### Assistant:", // Conversation format
                "[INST]",         // Start of new instruction
            ]
        } else if model_name_lower.contains("mistral") {
            vec![
                "</s>",           // End of sequence
                "[/INST]",        // End of instruction
                "<|im_end|>",     // ChatML format
            ]
        } else if model_name_lower.contains("vicuna") || model_name_lower.contains("wizard") {
            vec![
                "### Human:",     // Conversation format
                "### Assistant:", // Conversation format
                "USER:",          // Alternative format
                "ASSISTANT:",     // Alternative format
                "</s>",           // End of sequence
            ]
        } else if model_name_lower.contains("alpaca") {
            vec![
                "### Instruction:", // Alpaca format
                "### Response:",    // Alpaca format
                "### Input:",       // Alpaca format
                "</s>",            // End of sequence
            ]
        } else {
            // Generic/unknown model - use common stop sequences
            vec![
                "</s>",           // Most common end sequence
                "<|endoftext|>",  // GPT-style
                "<|im_end|>",     // ChatML
                "### Human:",     // Common conversation format
                "### Assistant:", // Common conversation format
                "[/INST]",        // Instruction format
                "<</SYS>>",       // System format
            ]
        }
    }

    // Helper function to clean up stop sequences from generated text
    fn clean_stop_sequences(&self, text: &str) -> String {
        let mut cleaned = text.to_string();
        let stop_sequences = self.get_stop_sequences();
        
        for stop_seq in &stop_sequences {
            if let Some(pos) = cleaned.find(stop_seq) {
                cleaned.truncate(pos);
                break; // Only remove the first occurrence to avoid over-truncation
            }
        }
        
        cleaned.trim().to_string()
    }

    // Download the Qwen 2.5 7B model if it doesn't exist
    fn download_qwen_model(model_path: &Path) -> Result<()> {
        use std::fs;
        use std::process::Command;
        
        const MODEL_URL: &str = "https://huggingface.co/Qwen/Qwen2.5-7B-Instruct-GGUF/resolve/main/qwen2.5-7b-instruct-q3_k_m.gguf";
        const MODEL_SIZE_MB: u64 = 3631; // Approximate size in MB
        
        // Create the parent directory if it doesn't exist
        if let Some(parent) = model_path.parent() {
            fs::create_dir_all(parent)?;
        }
        
        info!("Downloading Qwen 2.5 7B model (Q3_K_M quantization, ~3.5GB)...");
        info!("This is a one-time download that may take several minutes depending on your connection.");
        info!("Downloading to: {}", model_path.display());
        
        // Use curl with progress bar for download
        let output = Command::new("curl")
            .args([
                "-L",  // Follow redirects
                "-#",  // Show progress bar
                "-f",  // Fail on HTTP errors
                "-o", model_path.to_str().unwrap(),
                MODEL_URL,
            ])
            .output()?;
        
        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            
            // If curl is not available, provide alternative instructions
            if stderr.contains("command not found") || stderr.contains("not found") {
                error!("curl is not installed. Please install curl or manually download the model.");
                error!("Manual download instructions:");
                error!("1. Download from: {}", MODEL_URL);
                error!("2. Save to: {}", model_path.display());
                anyhow::bail!("curl not found - please install curl or download the model manually");
            }
            
            anyhow::bail!("Failed to download model: {}", stderr);
        }
        
        // Verify the file was created and has reasonable size
        let metadata = fs::metadata(model_path)?;
        let size_mb = metadata.len() / (1024 * 1024);
        
        if size_mb < MODEL_SIZE_MB - 100 {  // Allow some variance
            fs::remove_file(model_path).ok();  // Clean up partial download
            anyhow::bail!(
                "Downloaded file appears incomplete ({}MB vs expected ~{}MB). Please try again.",
                size_mb, MODEL_SIZE_MB
            );
        }
        
        info!("Successfully downloaded Qwen 2.5 7B model ({}MB)", size_mb);
        Ok(())
    }
}

#[async_trait::async_trait]
impl LLMProvider for EmbeddedProvider {
    async fn complete(&self, request: CompletionRequest) -> Result<CompletionResponse> {
        debug!(
            "Processing completion request with {} messages",
            request.messages.len()
        );

        let prompt = self.format_messages(&request.messages);
        let max_tokens = request.max_tokens.unwrap_or(self.max_tokens);
        let temperature = request.temperature.unwrap_or(self.temperature);

        debug!("Formatted prompt length: {} chars", prompt.len());

        let content = self
            .generate_completion(&prompt, max_tokens, temperature)
            .await?;

        // Estimate token usage (rough approximation)
        let prompt_tokens = (prompt.len() / 4) as u32; // Rough estimate: 4 chars per token
        let completion_tokens = (content.len() / 4) as u32;

        Ok(CompletionResponse {
            content,
            usage: Usage {
                prompt_tokens,
                completion_tokens,
                total_tokens: prompt_tokens + completion_tokens,
            },
            model: self.model_name.clone(),
        })
    }

    async fn stream(&self, request: CompletionRequest) -> Result<CompletionStream> {
        debug!(
            "Processing streaming request with {} messages",
            request.messages.len()
        );

        let prompt = self.format_messages(&request.messages);
        let max_tokens = request.max_tokens.unwrap_or(self.max_tokens);
        let temperature = request.temperature.unwrap_or(self.temperature);

        let (tx, rx) = mpsc::channel(100);
        let session = self.session.clone();
        let prompt = prompt.to_string();

        // Spawn streaming task
        tokio::task::spawn_blocking(move || {
            // Retry logic for acquiring the session lock
            let mut session_guard = None;
            for attempt in 0..5 {
                match session.try_lock() {
                    Ok(ctx) => {
                        session_guard = Some(ctx);
                        break;
                    }
                    Err(_) => {
                        if attempt < 4 {
                            debug!("Session busy, retrying in {}ms (attempt {}/5)", 100 * (attempt + 1), attempt + 1);
                            std::thread::sleep(std::time::Duration::from_millis(100 * (attempt + 1) as u64));
                        } else {
                            let _ = tx.blocking_send(Err(anyhow::anyhow!("Model is busy after 5 attempts, please try again")));
                            return;
                        }
                    }
                }
            }
            
            let mut session = match session_guard {
                Some(ctx) => ctx,
                None => {
                    let _ = tx.blocking_send(Err(anyhow::anyhow!("Failed to acquire session lock")));
                    return;
                }
            };

            // Set context to the prompt
            if let Err(e) = session.set_context(&prompt) {
                let _ = tx.blocking_send(Err(anyhow::anyhow!("Failed to set context: {}", e)));
                return;
            }

            // Create sampler with temperature
            let stages = vec![
                SamplerStage::Temperature(temperature),
                SamplerStage::TopK(40),
                SamplerStage::TopP(0.9),
            ];
            let sampler = StandardSampler::new_softmax(stages, 1);

            // Start completion
            let mut completion_handle = match session
                .start_completing_with(sampler, max_tokens as usize)
            {
                Ok(handle) => handle,
                Err(e) => {
                    let _ =
                        tx.blocking_send(Err(anyhow::anyhow!("Failed to start completion: {}", e)));
                    return;
                }
            };

            let mut accumulated_text = String::new();
            let mut token_count = 0;
            let mut unsent_tokens = String::new(); // Buffer for tokens we're holding back
            
            // Get stop sequences dynamically based on model type
            let stop_sequences = if prompt.contains("<|im_start|>") {
                // Qwen ChatML format detected
                vec!["<|im_end|>", "<|endoftext|>", "</s>", "<|im_start|>"]
            } else if prompt.contains("[INST]") || prompt.contains("<<SYS>>") {
                // Llama/CodeLlama format detected
                vec!["</s>", "[/INST]", "<</SYS>>", "[INST]", "<<SYS>>", "### Human:", "### Assistant:"]
            } else {
                // Generic format
                vec!["</s>", "<|endoftext|>", "<|im_end|>", "### Human:", "### Assistant:", "[/INST]", "<</SYS>>"]
            };

            // Stream tokens with proper limits
            while let Some(token) = completion_handle.next_token() {
                let token_string = session.model().token_to_piece(token);

                accumulated_text.push_str(&token_string);
                unsent_tokens.push_str(&token_string);
                token_count += 1;

                // Check if we've hit a complete stop sequence
                let mut hit_stop = false;
                for stop_seq in &stop_sequences {
                    if accumulated_text.contains(stop_seq) {
                        debug!("Hit complete stop sequence in streaming: {}", stop_seq);
                        hit_stop = true;
                        break;
                    }
                }

                if hit_stop {
                    // Before stopping, check if there might be an incomplete tool call
                    // Look for JSON tool call patterns that might be cut off by the stop sequence
                    let has_potential_tool_call = accumulated_text.contains(r#"{"tool":"#) || 
                                                  accumulated_text.contains(r#"{"{""tool"":"#) ||
                                                  accumulated_text.contains(r#"{{""tool"":"#);
                    
                    if has_potential_tool_call {
                        // Check if the tool call appears to be complete (has closing brace after the stop sequence)
                        let mut complete_tool_call = false;
                        for stop_seq in &stop_sequences {
                            if let Some(stop_pos) = accumulated_text.find(stop_seq) {
                                // Look for tool call pattern before the stop sequence
                                let before_stop = &accumulated_text[..stop_pos];
                                if let Some(tool_start) = before_stop.rfind(r#"{"tool":"#) {
                                    let tool_part = &before_stop[tool_start..];
                                    // Count braces to see if JSON is complete
                                    let open_braces = tool_part.matches('{').count();
                                    let close_braces = tool_part.matches('}').count();
                                    if open_braces > 0 && open_braces == close_braces {
                                        complete_tool_call = true;
                                        break;
                                    }
                                }
                            }
                        }
                        
                        // If tool call is incomplete, send the raw content including stop sequences
                        // so the main parser can handle it properly
                        if !complete_tool_call {
                            debug!("Found incomplete tool call, sending raw content with stop sequences");
                            let already_sent_len = accumulated_text.len() - unsent_tokens.len();
                            if accumulated_text.len() > already_sent_len {
                                let remaining_to_send = &accumulated_text[already_sent_len..];
                                if !remaining_to_send.is_empty() {
                                    let chunk = CompletionChunk {
                                        content: remaining_to_send.to_string(),
                                        finished: false,
                                        usage: None,
                                        tool_calls: None,
                                    };
                                    let _ = tx.blocking_send(Ok(chunk));
                                }
                            }
                            break;
                        }
                    }
                    
                    // Send any remaining clean content before stopping (original behavior)
                    let mut clean_accumulated = accumulated_text.clone();
                    for stop_seq in &stop_sequences {
                        if let Some(pos) = clean_accumulated.find(stop_seq) {
                            clean_accumulated.truncate(pos);
                            break;
                        }
                    }
                    
                    // Calculate what part we haven't sent yet
                    let already_sent_len = accumulated_text.len() - unsent_tokens.len();
                    if clean_accumulated.len() > already_sent_len {
                        let remaining_to_send = &clean_accumulated[already_sent_len..];
                        if !remaining_to_send.is_empty() {
                            let chunk = CompletionChunk {
                                content: remaining_to_send.to_string(),
                                finished: false,
                                usage: None,
                                tool_calls: None,
                            };
                            let _ = tx.blocking_send(Ok(chunk));
                        }
                    }
                    break;
                }

                // Check if we're building towards a stop sequence
                let mut might_be_stop = false;
                for stop_seq in &stop_sequences {
                    for i in 1..stop_seq.len() {
                        let partial = &stop_seq[..i];
                        if accumulated_text.ends_with(partial) {
                            debug!("Detected potential partial stop sequence: '{}'", partial);
                            might_be_stop = true;
                            break;
                        }
                    }
                    if might_be_stop {
                        break;
                    }
                }

                if might_be_stop {
                    // Hold back tokens, but only for a limited buffer size
                    if unsent_tokens.len() > 20 { // Don't hold back more than 20 characters
                        // Send the oldest part and keep only the recent part that might be a stop sequence
                        let to_send = &unsent_tokens[..unsent_tokens.len() - 10];
                        if !to_send.is_empty() {
                            let chunk = CompletionChunk {
                                content: to_send.to_string(),
                                finished: false,
                                usage: None,
                                tool_calls: None,
                            };
                            if tx.blocking_send(Ok(chunk)).is_err() {
                                break;
                            }
                        }
                        unsent_tokens = unsent_tokens[unsent_tokens.len() - 10..].to_string();
                    }
                    // Continue to next token without sending
                } else {
                    // No potential stop sequence, send all unsent tokens
                    if !unsent_tokens.is_empty() {
                        let chunk = CompletionChunk {
                            content: unsent_tokens.clone(),
                            finished: false,
                            usage: None,
                            tool_calls: None,
                        };
                        if tx.blocking_send(Ok(chunk)).is_err() {
                            break;
                        }
                        unsent_tokens.clear();
                    }
                }

                // Enforce token limit
                if token_count >= max_tokens as usize {
                    debug!("Reached max token limit in streaming: {}", max_tokens);
                    break;
                }
            }

            // Send final chunk
            let final_chunk = CompletionChunk {
                content: String::new(),
                finished: true,
                usage: None,  // Embedded models calculate usage differently
                tool_calls: None,
            };
            let _ = tx.blocking_send(Ok(final_chunk));
        });

        Ok(ReceiverStream::new(rx))
    }

    fn name(&self) -> &str {
        "embedded"
    }

    fn model(&self) -> &str {
        &self.model_name
    }
}

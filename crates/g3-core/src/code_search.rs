//! Code search functionality using ast-grep for syntax-aware semantic searches

use anyhow::{anyhow, Result};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::HashMap;
use std::process::Stdio;
use std::time::{Duration, Instant};
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::process::Command;
use tokio::sync::Semaphore;
use tracing::{debug, error, info, warn};

/// Maximum number of searches allowed per request
const MAX_SEARCHES: usize = 20;

/// Default timeout for individual searches in seconds
const DEFAULT_TIMEOUT_SECS: u64 = 60;

/// Default maximum concurrency
const DEFAULT_MAX_CONCURRENCY: usize = 4;

/// Default maximum matches per search
const DEFAULT_MAX_MATCHES: usize = 500;

/// Search specification for a single ast-grep search
#[derive(Debug, Clone, Deserialize)]
pub struct SearchSpec {
    pub name: String,
    pub mode: SearchMode,
    
    // Pattern mode fields
    pub pattern: Option<String>,
    pub language: Option<String>,
    
    // YAML mode fields
    pub rule_yaml: Option<String>,
    
    // Common fields
    pub paths: Option<Vec<String>>,
    pub globs: Option<Vec<String>>,
    pub json_style: Option<JsonStyle>,
    pub context: Option<u32>,
    pub threads: Option<u32>,
    pub include_metadata: Option<bool>,
    pub no_ignore: Option<Vec<NoIgnoreType>>,
    pub severity: Option<HashMap<String, SeverityLevel>>,
    pub timeout_secs: Option<u64>,
}

/// Search mode: pattern or yaml
#[derive(Debug, Clone, Deserialize, PartialEq)]
#[serde(rename_all = "lowercase")]
pub enum SearchMode {
    Pattern,
    Yaml,
}

/// JSON output style
#[derive(Debug, Clone, Deserialize, PartialEq)]
#[serde(rename_all = "lowercase")]
pub enum JsonStyle {
    Pretty,
    Stream,
    Compact,
}

impl Default for JsonStyle {
    fn default() -> Self {
        JsonStyle::Stream
    }
}

/// No-ignore types
#[derive(Debug, Clone, Deserialize, PartialEq)]
#[serde(rename_all = "lowercase")]
pub enum NoIgnoreType {
    Hidden,
    Dot,
    Exclude,
    Global,
    Parent,
    Vcs,
}

/// Severity levels for YAML rules
#[derive(Debug, Clone, Deserialize, PartialEq)]
#[serde(rename_all = "lowercase")]
pub enum SeverityLevel {
    Error,
    Warning,
    Info,
    Hint,
    Off,
}

/// Request structure for code search
#[derive(Debug, Deserialize)]
pub struct CodeSearchRequest {
    pub searches: Vec<SearchSpec>,
    pub max_concurrency: Option<usize>,
    pub max_matches_per_search: Option<usize>,
}

/// Result of a single search
#[derive(Debug, Serialize)]
pub struct SearchResult {
    pub name: String,
    pub mode: String,
    pub status: String,
    pub cmd: Vec<String>,
    pub match_count: Option<usize>,
    pub truncated: Option<bool>,
    pub matches: Option<Vec<Value>>,
    pub stderr: Option<String>,
    pub exit_code: Option<i32>,
    pub duration_ms: u64,
}

/// Summary of all searches
#[derive(Debug, Serialize)]
pub struct SearchSummary {
    pub completed: usize,
    pub total: usize,
    pub total_matches: usize,
    pub duration_ms: u64,
}

/// Complete response structure
#[derive(Debug, Serialize)]
pub struct CodeSearchResponse {
    pub summary: SearchSummary,
    pub searches: Vec<SearchResult>,
}

/// YAML rule structure for validation
#[derive(Debug, Deserialize)]
struct YamlRule {
    pub id: String,
    pub language: String,
    pub rule: Value,
}

/// Execute a batch of code searches using ast-grep
pub async fn execute_code_search(request: CodeSearchRequest) -> Result<CodeSearchResponse> {
    let start_time = Instant::now();
    
    // Validate request
    if request.searches.is_empty() {
        return Err(anyhow!("No searches specified"));
    }
    
    if request.searches.len() > MAX_SEARCHES {
        return Err(anyhow!(
            "Too many searches: {} (max: {})",
            request.searches.len(),
            MAX_SEARCHES
        ));
    }
    
    // Check if ast-grep is available
    check_ast_grep_available().await?;
    
    let max_concurrency = request.max_concurrency.unwrap_or(DEFAULT_MAX_CONCURRENCY);
    let max_matches = request.max_matches_per_search.unwrap_or(DEFAULT_MAX_MATCHES);
    
    // Create semaphore for concurrency control
    let semaphore = std::sync::Arc::new(Semaphore::new(max_concurrency));
    
    // Execute searches concurrently
    let mut tasks = Vec::new();

    for search in request.searches {
        let sem = semaphore.clone();
        let task = tokio::spawn(async move {
            let _permit = sem.acquire().await.unwrap();
            execute_single_search(search, max_matches).await
        });
        tasks.push(task);
    }
    
    // Wait for all searches to complete
    let mut results = Vec::new();
    let mut total_matches = 0;
    let mut completed = 0;
    
    for task in tasks {
        match task.await {
            Ok(result) => {
                if result.status == "ok" {
                    completed += 1;
                    if let Some(count) = result.match_count {
                        total_matches += count;
                    }
                }
                results.push(result);
            }
            Err(e) => {
                error!("Task join error: {}", e);
                // Create an error result
                results.push(SearchResult {
                    name: "unknown".to_string(),
                    mode: "unknown".to_string(),
                    status: "error".to_string(),
                    cmd: vec![],
                    match_count: None,
                    truncated: None,
                    matches: None,
                    stderr: Some(format!("Task execution error: {}", e)),
                    exit_code: None,
                    duration_ms: 0,
                });
            }
        }
    }
    
    let total_duration = start_time.elapsed();
    
    Ok(CodeSearchResponse {
        summary: SearchSummary {
            completed,
            total: results.len(),
            total_matches,
            duration_ms: total_duration.as_millis() as u64,
        },
        searches: results,
    })
}

/// Execute a single search
async fn execute_single_search(search: SearchSpec, max_matches: usize) -> SearchResult {
    let start_time = Instant::now();
    let timeout_secs = search.timeout_secs.unwrap_or(DEFAULT_TIMEOUT_SECS);
    
    // Validate the search specification
    if let Err(e) = validate_search_spec(&search) {
        return SearchResult {
            name: search.name,
            mode: format!("{:?}", search.mode).to_lowercase(),
            status: "error".to_string(),
            cmd: vec![],
            match_count: None,
            truncated: None,
            matches: None,
            stderr: Some(format!("Validation error: {}", e)),
            exit_code: None,
            duration_ms: start_time.elapsed().as_millis() as u64,
        };
    }
    
    // Build command
    let cmd_args = match build_ast_grep_command(&search) {
        Ok(args) => args,
        Err(e) => {
            return SearchResult {
                name: search.name,
                mode: format!("{:?}", search.mode).to_lowercase(),
                status: "error".to_string(),
                cmd: vec![],
                match_count: None,
                truncated: None,
                matches: None,
                stderr: Some(format!("Command build error: {}", e)),
                exit_code: None,
                duration_ms: start_time.elapsed().as_millis() as u64,
            };
        }
    };
    
    debug!("Executing ast-grep command: {:?}", cmd_args);

    // Execute with timeout
    let timeout_duration = Duration::from_secs(timeout_secs);
    
    match tokio::time::timeout(timeout_duration, run_ast_grep_command(&cmd_args)).await {
        Ok(Ok((stdout, stderr, exit_code))) => {
            let duration_ms = start_time.elapsed().as_millis() as u64;
            
            if exit_code == 0 {
                // Parse JSON output
                match parse_ast_grep_output(&stdout, max_matches) {
                    Ok((matches, truncated)) => {
                        SearchResult {
                            name: search.name,
                            mode: format!("{:?}", search.mode).to_lowercase(),
                            status: "ok".to_string(),
                            cmd: cmd_args,
                            match_count: Some(matches.len()),
                            truncated: Some(truncated),
                            matches: Some(matches),
                            stderr: if stderr.is_empty() { None } else { Some(stderr) },
                            exit_code: None,
                            duration_ms,
                        }
                    }
                    Err(e) => {
                        SearchResult {
                            name: search.name,
                            mode: format!("{:?}", search.mode).to_lowercase(),
                            status: "error".to_string(),
                            cmd: cmd_args,
                            match_count: None,
                            truncated: None,
                            matches: None,
                            stderr: Some(format!("JSON parse error: {}\nRaw output: {}", e, stdout)),
                            exit_code: Some(exit_code),
                            duration_ms,
                        }
                    }
                }
            } else {
                SearchResult {
                    name: search.name,
                    mode: format!("{:?}", search.mode).to_lowercase(),
                    status: "error".to_string(),
                    cmd: cmd_args,
                    match_count: None,
                    truncated: None,
                    matches: None,
                    stderr: Some(stderr),
                    exit_code: Some(exit_code),
                    duration_ms,
                }
            }
        }
        Ok(Err(e)) => {
            SearchResult {
                name: search.name,
                mode: format!("{:?}", search.mode).to_lowercase(),
                status: "error".to_string(),
                cmd: cmd_args,
                match_count: None,
                truncated: None,
                matches: None,
                stderr: Some(format!("Execution error: {}", e)),
                exit_code: None,
                duration_ms: start_time.elapsed().as_millis() as u64,
            }
        }
        Err(_) => {
            SearchResult {
                name: search.name,
                mode: format!("{:?}", search.mode).to_lowercase(),
                status: "timeout".to_string(),
                cmd: cmd_args,
                match_count: None,
                truncated: None,
                matches: None,
                stderr: Some(format!("Search timed out after {} seconds", timeout_secs)),
                exit_code: None,
                duration_ms: start_time.elapsed().as_millis() as u64,
            }
        }
    }
}

/// Validate a search specification
fn validate_search_spec(search: &SearchSpec) -> Result<()> {
    match search.mode {
        SearchMode::Pattern => {
            if search.pattern.is_none() || search.pattern.as_ref().unwrap().is_empty() {
                return Err(anyhow!("Pattern mode requires non-empty 'pattern' field"));
            }
        }
        SearchMode::Yaml => {
            let rule_yaml = search.rule_yaml.as_ref()
                .ok_or_else(|| anyhow!("YAML mode requires 'rule_yaml' field"))?;
            
            if rule_yaml.is_empty() {
                return Err(anyhow!("YAML mode requires non-empty 'rule_yaml' field"));
            }
            
            // Parse and validate YAML structure
            let parsed: YamlRule = serde_yaml::from_str(rule_yaml)
                .map_err(|e| anyhow!("Invalid YAML rule: {}", e))?;
            
            if parsed.id.is_empty() {
                return Err(anyhow!("YAML rule must have non-empty 'id' field"));
            }
            
            if parsed.language.is_empty() {
                return Err(anyhow!("YAML rule must have non-empty 'language' field"));
            }
            
            // Validate language is supported (basic check)
            validate_language(&parsed.language)?;
        }
    }
    
    // Validate context range
    if let Some(context) = search.context {
        if context > 20 {
            return Err(anyhow!("Context lines cannot exceed 20"));
        }
    }
    
    Ok(())
}

/// Validate that a language is supported by ast-grep
fn validate_language(language: &str) -> Result<()> {
    let supported_languages = [
        "rust", "javascript", "typescript", "python", "java", "c", "cpp", "csharp",
        "go", "html", "css", "json", "yaml", "xml", "bash", "kotlin", "swift",
        "php", "ruby", "scala", "dart", "lua", "r", "sql", "dockerfile",
        "Rust", "JavaScript", "TypeScript", "Python", "Java", "C", "Cpp", "CSharp",
        "Go", "Html", "Css", "Json", "Yaml", "Xml", "Bash", "Kotlin", "Swift",
        "Php", "Ruby", "Scala", "Dart", "Lua", "R", "Sql", "Dockerfile"
    ];
    
    if !supported_languages.contains(&language) {
        warn!("Language '{}' may not be supported by ast-grep", language);
    }
    
    Ok(())
}

/// Build ast-grep command arguments
fn build_ast_grep_command(search: &SearchSpec) -> Result<Vec<String>> {
    let mut args = vec!["ast-grep".to_string()];
    
    match search.mode {
        SearchMode::Pattern => {
            args.push("run".to_string());
            
            // Add pattern
            args.push("-p".to_string());
            args.push(search.pattern.as_ref().unwrap().clone());
            
            // Add language if specified
            if let Some(ref lang) = search.language {
                args.push("-l".to_string());
                args.push(lang.clone());
            }
        }
        SearchMode::Yaml => {
            args.push("scan".to_string());
            
            // Add inline rules
            args.push("--inline-rules".to_string());
            args.push(search.rule_yaml.as_ref().unwrap().clone());
            
            // Add include-metadata if requested
            if search.include_metadata.unwrap_or(false) {
                args.push("--include-metadata".to_string());
            }
            
            // Add severity overrides
            if let Some(ref severity_map) = search.severity {
                for (rule_id, severity) in severity_map {
                    match severity {
                        SeverityLevel::Error => {
                            args.push("--error".to_string());
                            args.push(rule_id.clone());
                        }
                        SeverityLevel::Warning => {
                            args.push("--warning".to_string());
                            args.push(rule_id.clone());
                        }
                        SeverityLevel::Info => {
                            args.push("--info".to_string());
                            args.push(rule_id.clone());
                        }
                        SeverityLevel::Hint => {
                            args.push("--hint".to_string());
                            args.push(rule_id.clone());
                        }
                        SeverityLevel::Off => {
                            args.push("--off".to_string());
                            args.push(rule_id.clone());
                        }
                    }
                }
            }
        }
    }
    
    // Add common arguments
    
    // Add globs if specified
    if let Some(ref globs) = search.globs {
        if !globs.is_empty() {
            args.push("--globs".to_string());
            args.push(globs.join(","));
        }
    }
    
    // Add context
    if let Some(context) = search.context {
        args.push("-C".to_string());
        args.push(context.to_string());
    }
    
    // Add threads
    if let Some(threads) = search.threads {
        args.push("-j".to_string());
        args.push(threads.to_string());
    }
    
    // Add JSON output style
    let json_style = search.json_style.as_ref().unwrap_or(&JsonStyle::Stream);
    let json_arg = match json_style {
        JsonStyle::Pretty => "--json=pretty",
        JsonStyle::Stream => "--json=stream",
        JsonStyle::Compact => "--json=compact",
    };
    args.push(json_arg.to_string());
    
    // Add no-ignore options
    if let Some(ref no_ignore_list) = search.no_ignore {
        for no_ignore_type in no_ignore_list {
            let flag = match no_ignore_type {
                NoIgnoreType::Hidden => "--no-ignore=hidden",
                NoIgnoreType::Dot => "--no-ignore=dot",
                NoIgnoreType::Exclude => "--no-ignore=exclude",
                NoIgnoreType::Global => "--no-ignore=global",
                NoIgnoreType::Parent => "--no-ignore=parent",
                NoIgnoreType::Vcs => "--no-ignore=vcs",
            };
            args.push(flag.to_string());
        }
    }
    
    // Add paths (default to current directory if none specified)
    if let Some(ref paths) = search.paths {
        if !paths.is_empty() {
            args.extend(paths.clone());
        } else {
            args.push(".".to_string());
        }
    } else {
        args.push(".".to_string());
    }
    
    Ok(args)
}

/// Run ast-grep command and capture output
async fn run_ast_grep_command(args: &[String]) -> Result<(String, String, i32)> {
    let mut cmd = Command::new(&args[0]);
    cmd.args(&args[1..]);
    cmd.stdout(Stdio::piped());
    cmd.stderr(Stdio::piped());
    
    debug!("Running command: {:?}", args);
    
    let mut child = cmd.spawn()
        .map_err(|e| anyhow!("Failed to spawn ast-grep process: {}", e))?;
    
    let stdout = child.stdout.take().unwrap();
    let stderr = child.stderr.take().unwrap();
    
    let stdout_reader = BufReader::new(stdout);
    let stderr_reader = BufReader::new(stderr);
    
    let stdout_task = tokio::spawn(async move {
        let mut lines = stdout_reader.lines();
        let mut output = String::new();
        while let Ok(Some(line)) = lines.next_line().await {
            if !output.is_empty() {
                output.push('\n');
            }
            output.push_str(&line);
        }
        output
    });
    
    let stderr_task = tokio::spawn(async move {
        let mut lines = stderr_reader.lines();
        let mut output = String::new();
        while let Ok(Some(line)) = lines.next_line().await {
            if !output.is_empty() {
                output.push('\n');
            }
            output.push_str(&line);
        }
        output
    });
    
    let status = child.wait().await
        .map_err(|e| anyhow!("Failed to wait for ast-grep process: {}", e))?;
    
    let stdout_output = stdout_task.await
        .map_err(|e| anyhow!("Failed to read stdout: {}", e))?;
    let stderr_output = stderr_task.await
        .map_err(|e| anyhow!("Failed to read stderr: {}", e))?;
    
    let exit_code = status.code().unwrap_or(-1);
    
    Ok((stdout_output, stderr_output, exit_code))
}

/// Parse ast-grep JSON output
fn parse_ast_grep_output(output: &str, max_matches: usize) -> Result<(Vec<Value>, bool)> {
    if output.trim().is_empty() {
        return Ok((vec![], false));
    }
    
    let mut matches = Vec::new();
    let mut truncated = false;
    
    // Handle stream format (line-delimited JSON)
    for line in output.lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        
        match serde_json::from_str::<Value>(line) {
            Ok(match_obj) => {
                if matches.len() >= max_matches {
                    truncated = true;
                    break;
                }
                matches.push(match_obj);
            }
            Err(e) => {
                debug!("Failed to parse JSON line '{}': {}", line, e);
                // Try to parse the entire output as a single JSON array
                match serde_json::from_str::<Vec<Value>>(output) {
                    Ok(array_matches) => {
                        let take_count = array_matches.len().min(max_matches);
                        let total_count = array_matches.len();
                        matches = array_matches.into_iter().take(take_count).collect();
                        truncated = take_count < total_count;
                        break;
                    }
                    Err(e2) => {
                        return Err(anyhow!(
                            "Failed to parse ast-grep output as line-delimited JSON or JSON array. Line error: {}, Array error: {}",
                            e, e2
                        ));
                    }
                }
            }
        }
    }
    
    Ok((matches, truncated))
}

/// Check if ast-grep is available and provide installation hints if not
async fn check_ast_grep_available() -> Result<()> {
    match Command::new("ast-grep")
        .arg("--version")
        .output()
        .await
    {
        Ok(output) => {
            if output.status.success() {
                let version = String::from_utf8_lossy(&output.stdout);
                info!("Found ast-grep: {}", version.trim());
                Ok(())
            } else {
                Err(anyhow!("ast-grep command failed: {}", String::from_utf8_lossy(&output.stderr)))
            }
        }
        Err(_) => {
            Err(anyhow!(
                "ast-grep not found. Please install it using one of these methods:\n\n\
                • Homebrew (macOS): brew install ast-grep\n\
                • MacPorts (macOS): sudo port install ast-grep\n\
                • Nix: nix-env -iA nixpkgs.ast-grep\n\
                • Cargo: cargo install ast-grep\n\
                • npm: npm install -g @ast-grep/cli\n\
                • pip: pip install ast-grep\n\n\
                For more installation options, visit: https://ast-grep.github.io/guide/quick-start.html"
            ))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    
    #[test]
    fn test_validate_pattern_search() {
        let search = SearchSpec {
            name: "test".to_string(),
            mode: SearchMode::Pattern,
            pattern: Some("fn $NAME() {}".to_string()),
            language: Some("rust".to_string()),
            rule_yaml: None,
            paths: None,
            globs: None,
            json_style: None,
            context: None,
            threads: None,
            include_metadata: None,
            no_ignore: None,
            severity: None,
            timeout_secs: None,
        };
        
        assert!(validate_search_spec(&search).is_ok());
    }
    
    #[test]
    fn test_validate_yaml_search() {
        let yaml_rule = r#"
id: test-rule
language: Rust
rule:
  pattern: "fn $NAME() {}"
"#;
        
        let search = SearchSpec {
            name: "test".to_string(),
            mode: SearchMode::Yaml,
            pattern: None,
            language: None,
            rule_yaml: Some(yaml_rule.to_string()),
            paths: None,
            globs: None,
            json_style: None,
            context: None,
            threads: None,
            include_metadata: None,
            no_ignore: None,
            severity: None,
            timeout_secs: None,
        };
        
        assert!(validate_search_spec(&search).is_ok());
    }
    
    #[test]
    fn test_build_pattern_command() {
        let search = SearchSpec {
            name: "test".to_string(),
            mode: SearchMode::Pattern,
            pattern: Some("fn $NAME() {}".to_string()),
            language: Some("rust".to_string()),
            rule_yaml: None,
            paths: Some(vec!["src/".to_string()]),
            globs: None,
            json_style: Some(JsonStyle::Stream),
            context: Some(2),
            threads: Some(4),
            include_metadata: None,
            no_ignore: None,
            severity: None,
            timeout_secs: None,
        };
        
        let cmd = build_ast_grep_command(&search).unwrap();
        
        assert_eq!(cmd[0], "ast-grep");
        assert_eq!(cmd[1], "run");
        assert!(cmd.contains(&"-p".to_string()));
        assert!(cmd.contains(&"fn $NAME() {}".to_string()));
        assert!(cmd.contains(&"-l".to_string()));
        assert!(cmd.contains(&"rust".to_string()));
        assert!(cmd.contains(&"--json=stream".to_string()));
        assert!(cmd.contains(&"-C".to_string()));
        assert!(cmd.contains(&"2".to_string()));
        assert!(cmd.contains(&"-j".to_string()));
        assert!(cmd.contains(&"4".to_string()));
        assert!(cmd.contains(&"src/".to_string()));
    }
    
    #[test]
    fn test_parse_stream_json() {
        let output = r#"{"file":"test.rs","text":"fn hello() {}"}
{"file":"test2.rs","text":"fn world() {}"}"#;
        
        let (matches, truncated) = parse_ast_grep_output(output, 10).unwrap();
        
        assert_eq!(matches.len(), 2);
        assert!(!truncated);
        assert_eq!(matches[0]["file"], "test.rs");
        assert_eq!(matches[1]["file"], "test2.rs");
    }
    
    #[test]
    fn test_parse_truncated_output() {
        let output = r#"{"file":"test1.rs","text":"fn a() {}"}
{"file":"test2.rs","text":"fn b() {}"}
{"file":"test3.rs","text":"fn c() {}"}"#;
        
        let (matches, truncated) = parse_ast_grep_output(output, 2).unwrap();
        
        assert_eq!(matches.len(), 2);
        assert!(truncated);
    }
}

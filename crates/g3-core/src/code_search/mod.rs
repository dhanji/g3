//! Code search functionality using tree-sitter for syntax-aware searches

use anyhow::Result;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

mod searcher;
pub use searcher::TreeSitterSearcher;

/// Request for batch code searches
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CodeSearchRequest {
    pub searches: Vec<SearchSpec>,
    #[serde(default = "default_concurrency")]
    pub max_concurrency: usize,
    #[serde(default = "default_max_matches")]
    pub max_matches_per_search: usize,
}

fn default_concurrency() -> usize {
    4
}

fn default_max_matches() -> usize {
    500
}

/// Individual search specification
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SearchSpec {
    /// Name/label for this search
    pub name: String,
    /// tree-sitter query (S-expression format)
    pub query: String,
    /// Language: "rust", "python", "javascript", "typescript"
    pub language: String,
    /// Paths to search (default: current directory)
    #[serde(default)]
    pub paths: Vec<String>,
    /// Lines of context around each match
    #[serde(default)]
    pub context_lines: usize,
}

/// Response containing all search results
#[derive(Debug, Serialize, Deserialize)]
pub struct CodeSearchResponse {
    pub searches: Vec<SearchResult>,
    pub total_matches: usize,
    pub total_files_searched: usize,
}

/// Result for a single search
#[derive(Debug, Serialize, Deserialize)]
pub struct SearchResult {
    pub name: String,
    pub matches: Vec<Match>,
    pub match_count: usize,
    pub files_searched: usize,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

/// A single match
#[derive(Debug, Serialize, Deserialize)]
pub struct Match {
    pub file: String,
    pub line: usize,
    pub column: usize,
    pub text: String,
    #[serde(skip_serializing_if = "HashMap::is_empty")]
    pub captures: HashMap<String, String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub context: Option<String>,
}

/// Main entry point for code search
pub async fn execute_code_search(request: CodeSearchRequest) -> Result<CodeSearchResponse> {
    let mut searcher = TreeSitterSearcher::new()?;
    searcher.execute_search(request).await
}

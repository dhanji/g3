use super::{CodeSearchRequest, CodeSearchResponse, Match, SearchResult, SearchSpec};
use anyhow::{anyhow, Result};
use std::collections::HashMap;
use std::fs;
use std::path::Path;
use tree_sitter::{Language, Parser, Query, QueryCursor};
use walkdir::WalkDir;

pub struct TreeSitterSearcher {
    parsers: HashMap<String, Parser>,
    languages: HashMap<String, Language>,
}

impl TreeSitterSearcher {
    pub fn new() -> Result<Self> {
        let mut parsers = HashMap::new();
        let mut languages = HashMap::new();

        // Initialize Rust
        {
            let mut parser = Parser::new();
            let language: Language = tree_sitter_rust::language().into();
            parser
                .set_language(&language)
                .map_err(|e| anyhow!("Failed to set Rust language: {}", e))?;
            parsers.insert("rust".to_string(), parser);
            languages.insert("rust".to_string(), language);
        }

        // Initialize Python
        {
            let mut parser = Parser::new();
            let language: Language = tree_sitter_python::language().into();
            parser
                .set_language(&language)
                .map_err(|e| anyhow!("Failed to set Python language: {}", e))?;
            parsers.insert("python".to_string(), parser);
            languages.insert("python".to_string(), language);
        }

        // Initialize JavaScript
        {
            let mut parser = Parser::new();
            let language: Language = tree_sitter_javascript::language().into();
            parser
                .set_language(&language)
                .map_err(|e| anyhow!("Failed to set JavaScript language: {}", e))?;
            parsers.insert("javascript".to_string(), parser);
            
            // Create separate parser for "js" alias
            let mut parser_js = Parser::new();
            parser_js.set_language(&language)
                .map_err(|e| anyhow!("Failed to set JavaScript language: {}", e))?;
            parsers.insert("js".to_string(), parser_js);
            languages.insert("javascript".to_string(), language.clone());
            languages.insert("js".to_string(), language.clone());
        }

        // Initialize TypeScript
        {
            let mut parser = Parser::new();
            let language: Language = tree_sitter_typescript::language_typescript().into();
            parser
                .set_language(&language)
                .map_err(|e| anyhow!("Failed to set TypeScript language: {}", e))?;
            parsers.insert("typescript".to_string(), parser);
            
            // Create separate parser for "ts" alias
            let mut parser_ts = Parser::new();
            parser_ts.set_language(&language)
                .map_err(|e| anyhow!("Failed to set TypeScript language: {}", e))?;
            parsers.insert("ts".to_string(), parser_ts);
            languages.insert("typescript".to_string(), language.clone());
            languages.insert("ts".to_string(), language.clone());
        }

        // Initialize Go
        {
            let mut parser = Parser::new();
            let language: Language = tree_sitter_go::language().into();
            parser
                .set_language(&language)
                .map_err(|e| anyhow!("Failed to set Go language: {}", e))?;
            parsers.insert("go".to_string(), parser);
            languages.insert("go".to_string(), language);
        }

        // Initialize Java
        {
            let mut parser = Parser::new();
            let language: Language = tree_sitter_java::language().into();
            parser
                .set_language(&language)
                .map_err(|e| anyhow!("Failed to set Java language: {}", e))?;
            parsers.insert("java".to_string(), parser);
            languages.insert("java".to_string(), language);
        }

        // Initialize C
        {
            let mut parser = Parser::new();
            let language: Language = tree_sitter_c::language().into();
            parser
                .set_language(&language)
                .map_err(|e| anyhow!("Failed to set C language: {}", e))?;
            parsers.insert("c".to_string(), parser);
            languages.insert("c".to_string(), language);
        }

        // Initialize C++
        {
            let mut parser = Parser::new();
            let language: Language = tree_sitter_cpp::language().into();
            parser
                .set_language(&language)
                .map_err(|e| anyhow!("Failed to set C++ language: {}", e))?;
            parsers.insert("cpp".to_string(), parser);
            languages.insert("cpp".to_string(), language);
        }

        // Initialize Kotlin
        {
            let mut parser = Parser::new();
            let language: Language = tree_sitter_kotlin::language().into();
            parser
                .set_language(&language)
                .map_err(|e| anyhow!("Failed to set Kotlin language: {}", e))?;
            parsers.insert("kotlin".to_string(), parser);
            languages.insert("kotlin".to_string(), language);
        }

        if parsers.is_empty() {
            return Err(anyhow!(
                "No language parsers available. Enable at least one language feature."
            ));
        }

        Ok(Self { parsers, languages })
    }

    pub async fn execute_search(
        &mut self,
        request: CodeSearchRequest,
    ) -> Result<CodeSearchResponse> {
        let mut all_results = Vec::new();
        let mut total_matches = 0;
        let mut total_files = 0;

        // Execute searches sequentially (could parallelize with tokio::spawn if needed)
        for spec in request.searches {
            let result = self
                .search_single(&spec, request.max_matches_per_search)
                .await;
            match result {
                Ok(search_result) => {
                    total_matches += search_result.match_count;
                    total_files += search_result.files_searched;
                    all_results.push(search_result);
                }
                Err(e) => {
                    all_results.push(SearchResult {
                        name: spec.name.clone(),
                        matches: vec![],
                        match_count: 0,
                        files_searched: 0,
                        error: Some(e.to_string()),
                    });
                }
            }
        }

        Ok(CodeSearchResponse {
            searches: all_results,
            total_matches,
            total_files_searched: total_files,
        })
    }

    async fn search_single(
        &mut self,
        spec: &SearchSpec,
        max_matches: usize,
    ) -> Result<SearchResult> {
        // Get parser and language
        let parser = self
            .parsers
            .get_mut(&spec.language)
            .ok_or_else(|| anyhow!("Unsupported language: {}", spec.language))?;
        let language = self
            .languages
            .get(&spec.language)
            .ok_or_else(|| anyhow!("Language not found: {}", spec.language))?;

        // Parse query
        let query = Query::new(language, &spec.query)
            .map_err(|e| anyhow!("Invalid query: {}", e))?;

        let mut matches = Vec::new();
        let mut files_searched = 0;

        // Determine search paths
        let search_paths = if spec.paths.is_empty() {
            vec![".".to_string()]
        } else {
            spec.paths.clone()
        };

        // Walk directories and search files
        for search_path in search_paths {
            for entry in WalkDir::new(&search_path)
                .follow_links(true)
                .into_iter()
                .filter_map(|e| e.ok())
            {
                if matches.len() >= max_matches {
                    break;
                }

                let path = entry.path();
                if !path.is_file() {
                    continue;
                }

                // Check file extension matches language
                if !Self::is_language_file(path, &spec.language) {
                    continue;
                }

                files_searched += 1;

                // Read and parse file
                if let Ok(source_code) = fs::read_to_string(path) {
                    if let Some(tree) = parser.parse(&source_code, None) {
                        let mut cursor = QueryCursor::new();
                        let query_matches = cursor.matches(
                            &query,
                            tree.root_node(),
                            source_code.as_bytes(),
                        );

                        for query_match in query_matches {
                            if matches.len() >= max_matches {
                                break;
                            }

                            // Extract captures
                            let mut captures_map = HashMap::new();
                            let mut match_text = String::new();
                            let mut match_line = 0;
                            let mut match_column = 0;

                            for capture in query_match.captures {
                                let capture_name = query.capture_names()[capture.index as usize];
                                let node = capture.node;
                                let text = &source_code[node.byte_range()];

                                captures_map.insert(capture_name.to_string(), text.to_string());

                                // Use first capture for position
                                if match_text.is_empty() {
                                    match_text = text.to_string();
                                    let start = node.start_position();
                                    match_line = start.row + 1;
                                    match_column = start.column + 1;
                                }
                            }

                            // Get context if requested
                            let context = if spec.context_lines > 0 {
                                Some(Self::get_context(
                                    &source_code,
                                    match_line,
                                    spec.context_lines,
                                ))
                            } else {
                                None
                            };

                            matches.push(Match {
                                file: path.display().to_string(),
                                line: match_line,
                                column: match_column,
                                text: match_text,
                                captures: captures_map,
                                context,
                            });
                        }
                    }
                }
            }
        }

        Ok(SearchResult {
            name: spec.name.clone(),
            match_count: matches.len(),
            files_searched,
            matches,
            error: None,
        })
    }

    fn is_language_file(path: &Path, language: &str) -> bool {
        let ext = path.extension().and_then(|e| e.to_str());
        match (language, ext) {
            ("rust", Some("rs")) => true,
            ("python", Some("py")) => true,
            ("javascript" | "js", Some("js" | "jsx" | "mjs")) => true,
            ("typescript" | "ts", Some("ts" | "tsx")) => true,
            ("go", Some("go")) => true,
            ("java", Some("java")) => true,
            ("c", Some("c" | "h")) => true,
            ("cpp", Some("cpp" | "cc" | "cxx" | "hpp" | "hxx" | "h")) => true,
            ("kotlin", Some("kt" | "kts")) => true,
            _ => false,
        }
    }

    fn get_context(source: &str, line: usize, context_lines: usize) -> String {
        let lines: Vec<&str> = source.lines().collect();
        // line is 1-indexed, convert to 0-indexed
        let line_idx = line.saturating_sub(1);
        // Get context_lines before and after
        let start = line_idx.saturating_sub(context_lines);
        let end = (line_idx + context_lines + 1).min(lines.len());
        lines[start..end].join("\n")
    }
}

//! Test Python async query

use tree_sitter::{Parser, Query, QueryCursor, Language};

fn main() -> anyhow::Result<()> {
    let source_code = r#"
def regular_function():
    pass

async def async_function():
    pass
"#;

    let mut parser = Parser::new();
    let language: Language = tree_sitter_python::language().into();
    parser.set_language(&language)?;

    let tree = parser.parse(source_code, None).unwrap();

    // Try different queries
    let queries = vec![
        "(function_definition (async) name: (identifier) @name)",
        "(function_definition (async))",
        "(async)",
    ];

    for query_str in queries {
        println!("\nTrying query: {}", query_str);
        match Query::new(&language, query_str) {
            Ok(query) => {
                let mut cursor = QueryCursor::new();
                let matches = cursor.matches(&query, tree.root_node(), source_code.as_bytes());
                let count = matches.count();
                println!("  ✓ Valid query, found {} matches", count);
            }
            Err(e) => {
                println!("  ✗ Invalid query: {}", e);
            }
        }
    }

    Ok(())
}

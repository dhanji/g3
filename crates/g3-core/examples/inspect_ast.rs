//! Inspect tree-sitter AST structure for Rust code

use tree_sitter::{Parser, Language};

fn print_tree(node: tree_sitter::Node, source: &str, indent: usize) {
    let indent_str = "  ".repeat(indent);
    let node_text = &source[node.byte_range()];
    let preview = if node_text.len() > 50 {
        format!("{}...", &node_text[..50])
    } else {
        node_text.to_string()
    };
    
    println!(
        "{}{} [{}:{}] '{}'",
        indent_str,
        node.kind(),
        node.start_position().row + 1,
        node.start_position().column + 1,
        preview.replace('\n', "\\n")
    );
    
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        print_tree(child, source, indent + 1);
    }
}

fn main() -> anyhow::Result<()> {
    let source_code = r#"
pub async fn example_async() {
    println!("Hello");
}

fn regular_function() {
    println!("Regular");
}

pub async fn another_async(x: i32) -> Result<(), ()> {
    Ok(())
}
"#;

    println!("Source code:");
    println!("{}", source_code);
    println!("\n{}", "=".repeat(80));
    println!("AST Structure:");
    println!("{}\n", "=".repeat(80));

    let mut parser = Parser::new();
    let language: Language = tree_sitter_rust::language().into();
    parser.set_language(&language)?;

    let tree = parser.parse(source_code, None).unwrap();
    print_tree(tree.root_node(), source_code, 0);

    Ok(())
}

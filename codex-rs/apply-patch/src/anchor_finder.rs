use std::path::Path;
use tree_sitter::Language;
use tree_sitter::Node;
use tree_sitter::Parser;
use tree_sitter::Tree;
use tree_sitter_javascript::LANGUAGE as JAVASCRIPT;
use tree_sitter_python::LANGUAGE as PYTHON;
use tree_sitter_rust::LANGUAGE as RUST;
use tree_sitter_typescript::LANGUAGE_TSX as TSX;
use tree_sitter_typescript::LANGUAGE_TYPESCRIPT as TYPESCRIPT;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct AnchorScope {
    pub start_line: usize,
    pub end_line: usize,
    /// Column offset (0-indexed) where the scope starts, used for indentation correction.
    pub start_column: usize,
}

#[derive(Clone)]
struct LanguageAdapter {
    language: Language,
    // No more hardcoded node_kinds. We use a more generic strategy.
}

pub(crate) struct AnchorFinder<'a> {
    bytes: &'a [u8],
    line_starts: Vec<usize>,
    line_count: usize,
    adapter: Option<LanguageAdapter>,
    tree: Option<Tree>,
}

impl<'a> AnchorFinder<'a> {
    pub(crate) fn new(path: &Path, source: &'a str, line_count: usize) -> Self {
        let adapter = language_adapter_for_path(path);
        let mut parser = Parser::new();
        let tree = adapter.as_ref().and_then(|adapter| {
            parser.set_language(&adapter.language).ok()?;
            parser.parse(source, None)
        });
        Self {
            bytes: source.as_bytes(),
            line_starts: build_line_starts(source),
            line_count,
            adapter,
            tree,
        }
    }

    pub(crate) fn scope_for_context(&self, context: &str) -> Option<AnchorScope> {
        self.scope_for_context_with_hint(context, None)
    }

    /// Find anchor scope with optional line hint for disambiguating duplicate symbols.
    /// When multiple symbols share the same name, prefers the one closest to `hint_line`.
    ///
    /// # Fallback Strategy (100+ Language Support)
    /// When no tree-sitter driver is available, uses "Pseudo-AST" based on:
    /// 1. Symbol anchoring via simple regex
    /// 2. Indent-based block scoping
    pub(crate) fn scope_for_context_with_hint(
        &self,
        context: &str,
        hint_line: Option<usize>,
    ) -> Option<AnchorScope> {
        if context.trim().is_empty() {
            return None;
        }

        // Primary path: Use tree-sitter AST if available (for Rust, Python, JS, TS)
        if let (Some(adapter), Some(tree)) = (self.adapter.as_ref(), self.tree.as_ref()) {
            // Use AST Fragment Parsing instead of keyword blacklists
            if let Some(symbol) = extract_symbol_via_ast(context, &adapter.language) {
                if let Some(node) = find_named_node_generically(
                    tree.root_node(),
                    self.bytes,
                    &symbol,
                    hint_line,
                    &self.line_starts,
                ) {
                    let mut start_line = line_for_byte(node.start_byte(), &self.line_starts)?;
                    let mut end_line =
                        line_for_byte(node.end_byte().saturating_sub(1), &self.line_starts)?;

                    let start_column = node.start_position().column;

                    if self.line_count == 0 {
                        return None;
                    }
                    let max_line = self.line_count.saturating_sub(1);
                    if start_line > max_line {
                        return None;
                    }
                    if end_line > max_line {
                        end_line = max_line;
                    }
                    if end_line < start_line {
                        start_line = end_line;
                    }
                    return Some(AnchorScope {
                        start_line,
                        end_line,
                        start_column,
                    });
                }
            }
        }

        // Fallback path: Universal Pseudo-AST (supports 100+ languages)
        // Uses indentation + symbol anchoring to simulate AST structure
        self.find_pseudo_scope(context, hint_line)
    }

    /// Universal Pseudo-AST Fallback for languages without tree-sitter support.
    ///
    /// This method provides "surgical precision" for 100+ languages using:
    /// 1. **Symbol Anchoring**: Extract symbol name from context line
    /// 2. **Indent-based Block Scoping**: Find block boundaries by indent level
    ///
    /// Works because almost all languages follow "parent indent < child indent" physical rule.
    fn find_pseudo_scope(&self, context: &str, hint_line: Option<usize>) -> Option<AnchorScope> {
        let cleaned = clean_patch_context(context);
        if cleaned.is_empty() {
            return None;
        }

        // Step 1: Extract symbol using simple regex (last identifier-like word)
        let symbol = extract_symbol_simple(&cleaned)?;

        // Step 2: Find symbol in source lines
        let source = std::str::from_utf8(self.bytes).ok()?;
        let lines: Vec<&str> = source.lines().collect();

        // Find candidate lines containing the symbol
        let candidates: Vec<(usize, &str)> = lines
            .iter()
            .enumerate()
            .filter_map(|(idx, &line)| {
                if line.contains(&symbol) {
                    Some((idx, line))
                } else {
                    None
                }
            })
            .collect();

        if candidates.is_empty() {
            return None;
        }

        // Use hint_line to select the best candidate
        let (start_line, start_line_content) = if let Some(hint) = hint_line {
            // Find the candidate closest to the hint
            candidates
                .into_iter()
                .min_by_key(|(idx, _)| idx.abs_diff(hint))
        } else {
            // Take the first occurrence
            candidates.into_iter().next()
        }?;

        // Step 3: Determine indent level of the start line
        let base_indent = count_indent(start_line_content);

        // Step 4: Find block end - scan forward until indent returns to <= base level
        let mut end_line = start_line;
        for (idx, line) in lines.iter().enumerate().skip(start_line + 1) {
            let current_indent = count_indent(line);

            // Skip empty lines
            if line.trim().is_empty() {
                end_line = idx;
                continue;
            }

            // If indent returns to base level or less, we've found the block end
            if current_indent <= base_indent {
                break;
            }

            end_line = idx;
        }

        // Ensure bounds
        let max_line = self.line_count.saturating_sub(1);
        if end_line > max_line {
            end_line = max_line;
        }

        Some(AnchorScope {
            start_line,
            end_line,
            start_column: base_indent,
        })
    }
}

fn language_adapter_for_path(path: &Path) -> Option<LanguageAdapter> {
    let ext = path.extension()?.to_str()?.to_ascii_lowercase();
    match ext.as_str() {
        "rs" => Some(LanguageAdapter { language: RUST.into() }),
        "py" => Some(LanguageAdapter { language: PYTHON.into() }),
        "js" | "jsx" => Some(LanguageAdapter { language: JAVASCRIPT.into() }),
        "ts" => Some(LanguageAdapter { language: TYPESCRIPT.into() }),
        "tsx" => Some(LanguageAdapter { language: TSX.into() }),
        _ => None,
    }
}

/// NEW: One-and-done implementation of symbol extraction.
/// No hardcoded keywords. No maintenance hell.
fn extract_symbol_via_ast(context: &str, language: &Language) -> Option<String> {
    let cleaned = clean_patch_context(context);
    if cleaned.is_empty() {
        return None;
    }
    let mut parser = Parser::new();
    parser.set_language(language).ok()?;

    // Treat the context line as a mini-source file.
    let tree = parser.parse(&cleaned, None)?;
    let root = tree.root_node();
    let bytes = cleaned.as_bytes();

    // Prefer declaration-like nodes with an explicit name field.
    if let Some(symbol) = select_symbol_candidate(root, bytes) {
        return Some(symbol);
    }

    // Fallback: first identifier-like node in the snippet.
    find_first_identifier(root, bytes)
}

/// Strip patch-specific prefixes and markers that would confuse tree-sitter.
/// Handles: @@, +, -, and leading/trailing whitespace.
fn clean_patch_context(context: &str) -> String {
    let mut s = context.trim();

    // Strip @@ prefix (common in unified diff context markers)
    if s.starts_with("@@") {
        s = s.strip_prefix("@@").unwrap_or(s).trim();
    }

    // Strip single +/- prefix (diff addition/removal markers)
    // But preserve if it looks like it's part of the code (e.g., "+=" operator)
    if s.starts_with('+') && !s.starts_with("++") && !s.starts_with("+=") {
        s = s.strip_prefix('+').unwrap_or(s).trim();
    }
    if s.starts_with('-') && !s.starts_with("--") && !s.starts_with("-=") {
        s = s.strip_prefix('-').unwrap_or(s).trim();
    }

    s.to_string()
}

#[derive(Debug, Clone)]
struct SymbolCandidate {
    name: String,
    score: i32,
    depth: usize,
    span: usize,
    start: usize,
}

fn select_symbol_candidate(root: Node<'_>, bytes: &[u8]) -> Option<String> {
    let mut best: Option<SymbolCandidate> = None;
    let mut stack = vec![(root, 0usize)];
    while let Some((node, depth)) = stack.pop() {
        if let Some(name) = node_symbol_name(node, bytes) {
            let name_len = name.len();
            let span = node.end_byte().saturating_sub(node.start_byte());
            let mut score = 10;
            let named_children = node.named_child_count() as i32;
            score += (named_children.min(6)) / 2;
            if span > name_len.saturating_add(1) {
                score += 2;
            }
            if node.start_byte() == 0 {
                score += 1;
            }
            if node.end_byte() == bytes.len() {
                score += 1;
            }
            if name_len <= 1 {
                score -= 2;
            }

            let candidate = SymbolCandidate {
                name,
                score,
                depth,
                span,
                start: node.start_byte(),
            };

            let should_replace = match best.as_ref() {
                None => true,
                Some(existing) => {
                    candidate.score > existing.score
                        || (candidate.score == existing.score
                            && (candidate.depth < existing.depth
                                || (candidate.depth == existing.depth
                                    && (candidate.span > existing.span
                                        || (candidate.span == existing.span
                                            && candidate.start < existing.start)))))
                }
            };

            if should_replace {
                best = Some(candidate);
            }
        }

        let mut cursor = node.walk();
        for child in node.children(&mut cursor) {
            stack.push((child, depth + 1));
        }
    }
    best.map(|candidate| candidate.name)
}

/// Field names used to extract symbol names from AST nodes.
/// Covers Rust, JavaScript/TypeScript, Python, and general patterns.
const NAME_FIELDS: &[&str] = &[
    // Common
    "name",
    "identifier",
    // Rust
    "declarator",
    "type",
    "pattern",
    // JavaScript/TypeScript
    "property_identifier",
    "shorthand_property_identifier",
    "type_identifier",
    // General
    "left",
    "binding",
];

fn node_symbol_name(node: Node<'_>, bytes: &[u8]) -> Option<String> {
    for field in NAME_FIELDS {
        if let Some(name_node) = node.child_by_field_name(field) {
            if let Ok(text) = name_node.utf8_text(bytes) {
                let trimmed = text.trim();
                if !trimmed.is_empty() {
                    return Some(trimmed.to_string());
                }
            }
        }
    }
    None
}

fn find_first_identifier(node: Node<'_>, bytes: &[u8]) -> Option<String> {
    if node.is_named() {
        let kind = node.kind();

        if kind.contains("identifier") || kind == "name" || kind == "property_identifier" {
            return node.utf8_text(bytes).ok().map(|s| s.to_string());
        }

        // For ERROR nodes, continue searching children (don't return early)
        // For non-ERROR nodes, also check for name field
        if kind != "ERROR" {
            if let Some(name_node) = node.child_by_field_name("name") {
                if let Ok(text) = name_node.utf8_text(bytes) {
                    let trimmed = text.trim();
                    if !trimmed.is_empty() {
                        return Some(trimmed.to_string());
                    }
                }
            }
        }
    }

    // DFS including inside ERROR nodes
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        if let Some(symbol) = find_first_identifier(child, bytes) {
            return Some(symbol);
        }
    }
    None
}

/// NEW: Generic node finder that doesn't care about specific 'Kinds'.
/// It searches for any node that has a name field matching our symbol.
fn find_named_node_generically<'a>(
    root: Node<'a>,
    bytes: &'a [u8],
    symbol: &str,
    hint_line: Option<usize>,
    line_starts: &[usize],
) -> Option<Node<'a>> {
    #[derive(Clone, Copy)]
    struct Candidate<'b> {
        node: Node<'b>,
        span: usize,
        line_distance: Option<usize>,
    }

    let mut stack = vec![root];
    let mut candidates: Vec<Candidate<'a>> = Vec::new();

    while let Some(node) = stack.pop() {
        // We look for nodes that have a name field matching our target symbol
        let mut found_match = false;
        for field in NAME_FIELDS {
            if let Some(name_node) = node.child_by_field_name(field) {
                if let Ok(text) = name_node.utf8_text(bytes) {
                    if text == symbol {
                        let span = node.end_byte().saturating_sub(node.start_byte());
                        let line_distance = hint_line.and_then(|hint| {
                            let node_start_line = line_for_byte(node.start_byte(), line_starts)?;
                            Some(node_start_line.abs_diff(hint))
                        });
                        candidates.push(Candidate {
                            node,
                            span,
                            line_distance,
                        });
                        found_match = true;
                        break;
                    }
                }
            }
        }

        // If we found a match for this node, don't descend into children
        // (prefer outer scope over nested definitions with same name)
        if !found_match {
            let mut cursor = node.walk();
            for child in node.children(&mut cursor) {
                stack.push(child);
            }
        }
    }

    // Sort candidates: prefer closer line, then smaller span
    candidates.sort_by(|a, b| {
        match (a.line_distance, b.line_distance) {
            (Some(dist_a), Some(dist_b)) => {
                dist_a.cmp(&dist_b).then_with(|| a.span.cmp(&b.span))
            }
            (Some(_), None) => std::cmp::Ordering::Less,
            (None, Some(_)) => std::cmp::Ordering::Greater,
            (None, None) => a.span.cmp(&b.span),
        }
    });

    candidates.first().map(|c| c.node)
}

fn build_line_starts(source: &str) -> Vec<usize> {
    let mut starts = Vec::new();
    starts.push(0);
    for (idx, b) in source.as_bytes().iter().enumerate() {
        if *b == b'\n' {
            starts.push(idx + 1);
        }
    }
    starts
}

fn line_for_byte(byte: usize, line_starts: &[usize]) -> Option<usize> {
    if line_starts.is_empty() {
        return None;
    }
    let mut lo = 0usize;
    let mut hi = line_starts.len();
    while lo < hi {
        let mid = (lo + hi) / 2;
        if line_starts[mid] <= byte {
            lo = mid + 1;
        } else {
            hi = mid;
        }
    }
    if lo == 0 { Some(0) } else { Some(lo - 1) }
}

// ============================================================================
// Universal Pseudo-AST Helper Functions (100+ Language Support)
// ============================================================================

/// Extract symbol name from context using simple regex.
/// Finds the last identifier-like word (alphanumeric + underscore).
/// This works for 100+ languages since almost all use similar identifier rules.
fn extract_symbol_simple(context: &str) -> Option<String> {
    // Clean the context first
    let cleaned = clean_patch_context(context);

    // Find all identifier-like sequences (letters, numbers, underscore)
    // Take the last one as the most likely symbol name
    let mut last_symbol = None;
    let mut current = String::new();

    for ch in cleaned.chars() {
        if ch.is_alphanumeric() || ch == '_' {
            current.push(ch);
        } else {
            if !current.is_empty() {
                // Filter out pure numbers and very short strings
                if current.chars().any(|c| c.is_alphabetic()) && current.len() >= 2 {
                    last_symbol = Some(current.clone());
                }
                current.clear();
            }
        }
    }

    // Don't forget the last symbol
    if !current.is_empty() && current.chars().any(|c| c.is_alphabetic()) && current.len() >= 2 {
        last_symbol = Some(current);
    }

    last_symbol
}

/// Count the indentation level of a line (spaces and tabs).
/// Tabs count as `tab_width` columns (default 4).
fn count_indent(line: &str) -> usize {
    const TAB_WIDTH: usize = 4;
    let mut indent = 0;
    for ch in line.chars() {
        match ch {
            ' ' => indent += 1,
            '\t' => indent += TAB_WIDTH,
            _ => break,
        }
    }
    indent
}

#[cfg(test)]
mod tests {
    use super::extract_symbol_via_ast;
    use super::AnchorFinder;
    use std::path::PathBuf;
    use tree_sitter::Language;
    use tree_sitter_javascript::LANGUAGE as JAVASCRIPT;
    use tree_sitter_python::LANGUAGE as PYTHON;
    use tree_sitter_rust::LANGUAGE as RUST;
    use tree_sitter_typescript::LANGUAGE_TYPESCRIPT as TYPESCRIPT;

    // === Existing tests ===

    #[test]
    fn extract_symbol_rust_fn() {
        let language: Language = RUST.into();
        let symbol = extract_symbol_via_ast("fn build_window() -> Self {", &language);
        assert_eq!(symbol.as_deref(), Some("build_window"));
    }

    #[test]
    fn extract_symbol_rust_impl_type() {
        let language: Language = RUST.into();
        let symbol = extract_symbol_via_ast("impl SessionWindow {", &language);
        assert_eq!(symbol.as_deref(), Some("SessionWindow"));
    }

    #[test]
    fn extract_symbol_python_class() {
        let language: Language = PYTHON.into();
        let symbol = extract_symbol_via_ast("class SessionWindow:", &language);
        assert_eq!(symbol.as_deref(), Some("SessionWindow"));
    }

    #[test]
    fn extract_symbol_js_function() {
        let language: Language = JAVASCRIPT.into();
        let symbol = extract_symbol_via_ast("function renderWindow() {", &language);
        assert_eq!(symbol.as_deref(), Some("renderWindow"));
    }

    #[test]
    fn extract_symbol_fallback_identifier() {
        let language: Language = RUST.into();
        let symbol = extract_symbol_via_ast("let alpha = beta;", &language);
        assert_eq!(symbol.as_deref(), Some("alpha"));
    }

    // === Phase 1: Expanded Field Names Tests ===

    #[test]
    fn extract_symbol_typescript_interface() {
        let language: Language = TYPESCRIPT.into();
        let symbol = extract_symbol_via_ast("interface UserProfile {", &language);
        assert_eq!(symbol.as_deref(), Some("UserProfile"));
    }

    #[test]
    fn extract_symbol_rust_trait() {
        let language: Language = RUST.into();
        let symbol = extract_symbol_via_ast("pub trait AsRef<T> {", &language);
        assert_eq!(symbol.as_deref(), Some("AsRef"));
    }

    #[test]
    fn extract_symbol_js_class_method() {
        let language: Language = JAVASCRIPT.into();
        let symbol = extract_symbol_via_ast("  handleClick(event) {", &language);
        assert_eq!(symbol.as_deref(), Some("handleClick"));
    }

    #[test]
    fn extract_symbol_typescript_type_alias() {
        let language: Language = TYPESCRIPT.into();
        let symbol = extract_symbol_via_ast("type Status = 'active' | 'inactive';", &language);
        assert_eq!(symbol.as_deref(), Some("Status"));
    }

    // === Phase 2: ERROR Node Tolerance Tests ===

    #[test]
    fn extract_symbol_rust_fn_incomplete() {
        let language: Language = RUST.into();
        // Incomplete function - tree-sitter creates ERROR nodes
        let symbol = extract_symbol_via_ast("fn incomplete_fn(", &language);
        assert_eq!(symbol.as_deref(), Some("incomplete_fn"));
    }

    #[test]
    fn extract_symbol_python_incomplete_class() {
        let language: Language = PYTHON.into();
        // Incomplete class definition
        let symbol = extract_symbol_via_ast("class IncompleteClass", &language);
        assert_eq!(symbol.as_deref(), Some("IncompleteClass"));
    }

    #[test]
    fn extract_symbol_js_arrow_incomplete() {
        let language: Language = JAVASCRIPT.into();
        // Incomplete arrow function
        let symbol = extract_symbol_via_ast("const myFunc =", &language);
        assert_eq!(symbol.as_deref(), Some("myFunc"));
    }

    // === Phase 3: Line Affinity Tests ===

    #[test]
    fn extract_symbol_prefers_closer_line() {
        // Source with duplicate function names at different locations
        let source = r#"fn main() {
    let x = 1;
}

fn duplicate_name() {
    // First definition at line 5
}

fn other_func() {
    // Some other function
}

fn duplicate_name() {
    // Second definition at line 13
}
"#;
        let line_count = source.lines().count();
        let finder = AnchorFinder::new(&PathBuf::from("test.rs"), source, line_count);

        // Without hint, should find first occurrence (smaller span preferred)
        let scope_no_hint = finder.scope_for_context("fn duplicate_name() {");
        assert!(scope_no_hint.is_some());
        let scope = scope_no_hint.unwrap();
        // Should find line 4 (0-indexed) = "fn duplicate_name()" first occurrence
        assert_eq!(scope.start_line, 4);

        // With hint pointing to second occurrence (line 12, 0-indexed)
        let scope_with_hint = finder.scope_for_context_with_hint("fn duplicate_name() {", Some(12));
        assert!(scope_with_hint.is_some());
        let scope = scope_with_hint.unwrap();
        // Should find line 12 (0-indexed) = "fn duplicate_name()" second occurrence
        assert_eq!(scope.start_line, 12);
    }

    #[test]
    fn line_affinity_single_occurrence_unchanged() {
        let source = r#"fn unique_function() {
    // Only one definition
}
"#;
        let line_count = source.lines().count();
        let finder = AnchorFinder::new(&PathBuf::from("test.rs"), source, line_count);

        let scope_no_hint = finder.scope_for_context("fn unique_function() {");
        let scope_with_hint = finder.scope_for_context_with_hint("fn unique_function() {", Some(5));

        // Both should find the same single occurrence
        assert_eq!(scope_no_hint, scope_with_hint);
    }

    // === Patch Context Cleaning Tests ===

    #[test]
    fn extract_symbol_with_at_at_prefix() {
        let language: Language = RUST.into();
        // Unified diff context marker
        let symbol = extract_symbol_via_ast("@@ fn my_function() {", &language);
        assert_eq!(symbol.as_deref(), Some("my_function"));
    }

    #[test]
    fn extract_symbol_with_plus_prefix() {
        let language: Language = RUST.into();
        // Diff addition marker
        let symbol = extract_symbol_via_ast("+fn added_function() {", &language);
        assert_eq!(symbol.as_deref(), Some("added_function"));
    }

    #[test]
    fn extract_symbol_with_minus_prefix() {
        let language: Language = RUST.into();
        // Diff removal marker
        let symbol = extract_symbol_via_ast("-fn removed_function() {", &language);
        assert_eq!(symbol.as_deref(), Some("removed_function"));
    }

    #[test]
    fn extract_symbol_preserves_plus_equals() {
        let language: Language = RUST.into();
        // += operator should not be stripped
        let symbol = extract_symbol_via_ast("let x += 1;", &language);
        assert_eq!(symbol.as_deref(), Some("x"));
    }

    #[test]
    fn extract_symbol_preserves_minus_equals() {
        let language: Language = RUST.into();
        // -= operator should not be stripped
        let symbol = extract_symbol_via_ast("let y -= 1;", &language);
        assert_eq!(symbol.as_deref(), Some("y"));
    }

    #[test]
    fn extract_symbol_with_combined_markers() {
        let language: Language = RUST.into();
        // @@ + combination
        let symbol = extract_symbol_via_ast("@@ +fn combined() {", &language);
        assert_eq!(symbol.as_deref(), Some("combined"));
    }

    #[test]
    fn extract_symbol_python_with_at_at() {
        let language: Language = PYTHON.into();
        let symbol = extract_symbol_via_ast("@@ class MyClass:", &language);
        assert_eq!(symbol.as_deref(), Some("MyClass"));
    }

    #[test]
    fn extract_symbol_js_with_plus() {
        let language: Language = JAVASCRIPT.into();
        let symbol = extract_symbol_via_ast("+function handleClick() {", &language);
        assert_eq!(symbol.as_deref(), Some("handleClick"));
    }

    // === Universal Pseudo-AST Tests (100+ Language Support) ===

    #[test]
    fn test_extract_symbol_simple_function() {
        let symbol = super::extract_symbol_simple("fn my_function() {");
        assert_eq!(symbol.as_deref(), Some("my_function"));
    }

    #[test]
    fn test_extract_symbol_simple_class() {
        let symbol = super::extract_symbol_simple("class MyClass:");
        assert_eq!(symbol.as_deref(), Some("MyClass"));
    }

    #[test]
    fn test_extract_symbol_simple_last_identifier() {
        // Should take the last identifier-like word
        let symbol = super::extract_symbol_simple("func processData(input string)");
        assert_eq!(symbol.as_deref(), Some("string")); // last identifier
    }

    #[test]
    fn test_extract_symbol_simple_with_patch_markers() {
        let symbol = super::extract_symbol_simple("+func process() {");
        assert_eq!(symbol.as_deref(), Some("process"));
    }

    #[test]
    fn test_count_indent_spaces() {
        assert_eq!(super::count_indent("    let x = 1;"), 4);
        assert_eq!(super::count_indent("  foo"), 2);
        assert_eq!(super::count_indent("no indent"), 0);
    }

    #[test]
    fn test_count_indent_tabs() {
        assert_eq!(super::count_indent("\tlet x = 1;"), 4); // 1 tab = 4 cols
        assert_eq!(super::count_indent("\t\tfoo"), 8); // 2 tabs = 8 cols
    }

    #[test]
    fn test_count_indent_mixed() {
        assert_eq!(super::count_indent("\t  let x = 1;"), 6); // 1 tab + 2 spaces = 6 cols
    }

    #[test]
    fn test_pseudo_scope_fallback_for_unknown_language() {
        // Test that Pseudo-AST works for .lua (not in tree-sitter support)
        let source = r#"function calculate_sum(a, b)
    local result = a + b
    return result
end

function main()
    print(calculate_sum(1, 2))
end
"#;
        let line_count = source.lines().count();
        let finder = AnchorFinder::new(&PathBuf::from("test.lua"), source, line_count);

        // Should find the scope of calculate_sum function
        let scope = finder.scope_for_context_with_hint("function calculate_sum", None);
        assert!(scope.is_some());
        let scope = scope.unwrap();
        assert_eq!(scope.start_line, 0);
        assert!(scope.end_line >= 2); // Should include at least 3 lines
    }

    #[test]
    fn test_pseudo_scope_indent_based() {
        // Test Python-like indent-based scoping
        let source = r#"def outer():
    x = 1
    def inner():
        y = 2
    z = 3
"#;
        let line_count = source.lines().count();
        let finder = AnchorFinder::new(&PathBuf::from("test.py"), source, line_count);

        // Should use tree-sitter since .py is supported
        // But if tree-sitter fails, should fall back to pseudo-AST
        let scope = finder.scope_for_context_with_hint("def inner", None);
        assert!(scope.is_some());
    }
}

//! Phase 3: Syntax Validation Guard
//!
//! This module implements Layer 4 safeguards:
//! 1. Error Delta Analysis - compare before/after syntax errors
//! 2. Transactional Atomicity - all-or-nothing multi-file patches
//! 3. Enhanced Diagnostics for agent feedback

use std::path::{Path, PathBuf};
use tree_sitter::{Language, Node, Parser};

/// Supported languages for syntax validation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SupportedLanguage {
    Rust,
    Python,
    JavaScript,
    TypeScript,
    TypeScriptTsx,
    Unknown,
}

impl SupportedLanguage {
    /// Detect language from file extension.
    pub fn from_path(path: &Path) -> Self {
        let ext = path
            .extension()
            .and_then(|e| e.to_str())
            .map(|e| e.to_ascii_lowercase());

        match ext.as_deref() {
            Some("rs") => Self::Rust,
            Some("py") => Self::Python,
            Some("js") | Some("jsx") | Some("mjs") | Some("cjs") => Self::JavaScript,
            Some("ts") => Self::TypeScript,
            Some("tsx") => Self::TypeScriptTsx,
            _ => Self::Unknown,
        }
    }

    /// Get the tree-sitter language for this language.
    pub fn language(&self) -> Option<Language> {
        match self {
            Self::Rust => Some(tree_sitter_rust::LANGUAGE.into()),
            Self::Python => Some(tree_sitter_python::LANGUAGE.into()),
            Self::JavaScript => Some(tree_sitter_javascript::LANGUAGE.into()),
            Self::TypeScript => Some(tree_sitter_typescript::LANGUAGE_TYPESCRIPT.into()),
            Self::TypeScriptTsx => Some(tree_sitter_typescript::LANGUAGE_TSX.into()),
            Self::Unknown => None,
        }
    }
}

/// Information about a syntax error found in the AST.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SyntaxError {
    /// Line number (0-indexed)
    pub line: usize,
    /// Column number (0-indexed)
    pub column: usize,
    /// Byte offset where the error starts
    pub start_byte: usize,
    /// Byte offset where the error ends
    pub end_byte: usize,
    /// The kind of node that contains the error (if known)
    pub node_kind: Option<String>,
    /// Surrounding context (the line content)
    pub context: Option<String>,
}

/// Result of syntax validation for a single file.
#[derive(Debug, Clone)]
pub struct ValidationResult {
    pub path: PathBuf,
    pub language: SupportedLanguage,
    pub before_errors: Vec<SyntaxError>,
    pub after_errors: Vec<SyntaxError>,
    pub is_valid: bool,
    pub error_delta: ErrorDelta,
}

/// The change in error count between before and after.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ErrorDelta {
    /// Number of new errors introduced by the patch
    pub new_errors: usize,
    /// Number of errors that were fixed by the patch
    pub fixed_errors: usize,
    /// Net change in error count (positive = more errors)
    pub net_change: i64,
}

impl ErrorDelta {
    pub fn is_degradation(&self) -> bool {
        self.net_change > 0
    }
}

/// Diagnostic information for agent feedback.
#[derive(Debug, Clone)]
pub struct DiagnosticInfo {
    pub path: PathBuf,
    pub language: Option<SupportedLanguage>,
    pub stage: Option<String>,
    pub tag: Option<String>,
    pub context: Option<String>,
    pub issue: String,
    pub evidence: String,
    pub suggestion: String,
    pub next_action: Option<String>,
}

impl std::fmt::Display for DiagnosticInfo {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        writeln!(f, "Error: Syntax Integrity Check Failed")?;
        writeln!(f, "Path: {}", self.path.display())?;
        if let Some(stage) = &self.stage {
            writeln!(f, "Stage: {stage}")?;
        }
        if let Some(tag) = &self.tag {
            writeln!(f, "Tag: {tag}")?;
        }
        if let Some(language) = self.language {
            writeln!(f, "Language: {:?}", language)?;
        }
        if let Some(ctx) = &self.context {
            writeln!(f, "Context: {ctx}")?;
        }
        writeln!(f, "Issue: {}", self.issue)?;
        writeln!(f, "Evidence: {}", self.evidence)?;
        writeln!(f, "Suggestion: {}", self.suggestion)?;
        if let Some(next_action) = &self.next_action {
            writeln!(f, "Next action: {next_action}")?;
        }
        Ok(())
    }
}

/// Parse source code and extract ERROR nodes.
pub fn extract_errors(source: &str, language: SupportedLanguage) -> Option<Vec<SyntaxError>> {
    let lang = language.language()?;
    let mut parser = Parser::new();
    parser.set_language(&lang).ok()?;
    let tree = parser.parse(source, None)?;

    let mut errors = Vec::new();
    collect_error_nodes(tree.root_node(), source, &mut errors);
    Some(errors)
}

fn collect_error_nodes(node: Node, source: &str, errors: &mut Vec<SyntaxError>) {
    if node.is_error() || node.is_missing() {
        let start_pos = node.start_position();
        let line_content = source.lines().nth(start_pos.row).map(|s| s.to_string());

        errors.push(SyntaxError {
            line: start_pos.row,
            column: start_pos.column,
            start_byte: node.start_byte(),
            end_byte: node.end_byte(),
            node_kind: Some(node.kind().to_string()),
            context: line_content,
        });
    }

    // Recurse into children
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        collect_error_nodes(child, source, errors);
    }
}

/// Compare errors before and after a patch.
pub fn compute_error_delta(
    before_errors: &[SyntaxError],
    after_errors: &[SyntaxError],
    patch_byte_range: Option<(usize, usize)>,
) -> ErrorDelta {
    // Find errors that are new (not in before)
    let new_errors: Vec<&SyntaxError> = after_errors
        .iter()
        .filter(|e| {
            // Check if this error didn't exist before
            !before_errors.iter().any(|be| {
                be.line == e.line && be.column == e.column && be.node_kind == e.node_kind
            }) && // If we have a patch byte range, check if the error is in the patched region
            patch_byte_range.map_or(true, |(start, end)| {
                e.start_byte >= start && e.end_byte <= end
            })
        })
        .collect();

    // Find errors that were fixed (in before but not in after)
    let fixed_errors: Vec<&SyntaxError> = before_errors
        .iter()
        .filter(|e| {
            !after_errors
                .iter()
                .any(|ae| ae.line == e.line && ae.column == e.column && ae.node_kind == e.node_kind)
        })
        .collect();

    ErrorDelta {
        new_errors: new_errors.len(),
        fixed_errors: fixed_errors.len(),
        net_change: new_errors.len() as i64 - fixed_errors.len() as i64,
    }
}

// =============================================================================
// Generic Robustness Probe (Phase 2 Audit Improvement)
// =============================================================================
// For languages without tree-sitter support, perform basic physical integrity
// checks that are language-agnostic but still catch common LLM mistakes.

/// Result of the Generic Robustness Probe.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RobustnessIssue {
    /// Line number where the issue was found (0-indexed)
    pub line: usize,
    /// Description of the issue
    pub description: String,
    /// Evidence (the problematic content)
    pub evidence: String,
}

/// Check delimiter balance for {}, [], () in the content.
///
/// Returns a list of issues found. An empty list means the content passes
/// the balance check.
pub fn check_delimiter_balance(content: &str) -> Vec<RobustnessIssue> {
    let mut issues = Vec::new();
    let mut stack: Vec<(char, usize)> = Vec::new(); // (delimiter, line_number)

    let pairs = [('{', '}'), ('[', ']'), ('(', ')')];

    for (line_num, line) in content.lines().enumerate() {
        // Skip string literals and comments (simple heuristic)
        let mut in_string = false;
        let mut string_char = ' ';
        let mut in_line_comment = false;

        for (col, ch) in line.chars().enumerate() {
            // Handle line comments (// or #)
            if !in_string && !in_line_comment {
                if ch == '/' && line.chars().nth(col + 1) == Some('/') {
                    in_line_comment = true;
                    continue;
                }
                if ch == '#' {
                    in_line_comment = true;
                    continue;
                }
            }

            if in_line_comment {
                continue;
            }

            // Handle string literals
            if !in_string && (ch == '"' || ch == '\'' || ch == '`') {
                in_string = true;
                string_char = ch;
                continue;
            }
            if in_string && ch == string_char {
                // Check for escape
                let prev = line.chars().nth(col.saturating_sub(1));
                if prev != Some('\\') {
                    in_string = false;
                }
                continue;
            }

            if in_string {
                continue;
            }

            // Check delimiters
            if ['{', '[', '('].contains(&ch) {
                stack.push((ch, line_num));
            } else if ['}', ']', ')'].contains(&ch) {
                let expected_open = pairs
                    .iter()
                    .find(|(_, close)| *close == ch)
                    .map(|(open, _)| *open);

                match (stack.pop(), expected_open) {
                    (Some((open, _)), Some(expected)) if open == expected => {
                        // Matched correctly
                    }
                    (Some((open, open_line)), Some(expected)) => {
                        issues.push(RobustnessIssue {
                            line: line_num,
                            description: format!(
                                "Mismatched delimiter: expected '{}' to close '{}' at line {}, found '{}'",
                                expected, open, open_line + 1, ch
                            ),
                            evidence: line.to_string(),
                        });
                    }
                    (None, Some(_)) => {
                        issues.push(RobustnessIssue {
                            line: line_num,
                            description: format!(
                                "Unexpected closing delimiter '{}' with no matching opener",
                                ch
                            ),
                            evidence: line.to_string(),
                        });
                    }
                    _ => {}
                }
            }
        }
    }

    // Check for unclosed delimiters
    for (open, open_line) in stack {
        let close = pairs.iter().find(|(o, _)| *o == open).map(|(_, c)| *c);
        issues.push(RobustnessIssue {
            line: open_line,
            description: format!(
                "Unclosed delimiter '{}' - missing '{}'",
                open,
                close.unwrap_or('?')
            ),
            evidence: content.lines().nth(open_line).unwrap_or("").to_string(),
        });
    }

    issues
}

/// Check for Git conflict markers in the content.
///
/// Returns a list of issues found for any conflict markers detected.
pub fn check_conflict_markers(content: &str) -> Vec<RobustnessIssue> {
    let mut issues = Vec::new();

    let conflict_markers = [
        ("<<<<<<<", "Git conflict start marker"),
        ("=======", "Git conflict separator marker"),
        // Note: >>>>>>> is also a conflict marker but can appear in docstrings/comments legitimately
        // We check it only when preceded by conflict context
    ];

    for (line_num, line) in content.lines().enumerate() {
        let trimmed = line.trim();

        for (marker, description) in &conflict_markers {
            if trimmed.starts_with(marker) {
                // Check if it's likely a real conflict marker (7 chars, possibly with trailing info)
                if trimmed.len() <= 50 {
                    // Reasonable length for conflict marker
                    issues.push(RobustnessIssue {
                        line: line_num,
                        description: (*description).to_string(),
                        evidence: line.to_string(),
                    });
                }
            }
        }
    }

    issues
}

/// Check for embedded diff markers that indicate malformed patch application.
///
/// This detects the pattern where `+` characters from diff format have been
/// incorrectly embedded into source code (e.g., `code;+    more_code;`).
pub fn check_embedded_diff_markers(content: &str) -> Vec<RobustnessIssue> {
    let mut issues = Vec::new();

    for (line_num, line) in content.lines().enumerate() {
        // Pattern: semicolon or brace followed by `+` and then more code
        // This indicates squashed diff lines: `original;+    added`
        if let Some(pos) = line.find(";+") {
            // Check if there's actual code after the `+`
            let after = &line[pos + 2..];
            if after.trim().len() > 2 && after.chars().next().map_or(false, |c| c.is_whitespace()) {
                issues.push(RobustnessIssue {
                    line: line_num,
                    description: "Embedded diff marker detected - `+` character appears to be from malformed patch".to_string(),
                    evidence: line.to_string(),
                });
            }
        }

        // Also check for `}+` pattern
        if let Some(pos) = line.find("}+") {
            let after = &line[pos + 2..];
            if after.trim().len() > 2 && after.chars().next().map_or(false, |c| c.is_whitespace()) {
                issues.push(RobustnessIssue {
                    line: line_num,
                    description:
                        "Embedded diff marker detected - `+` character after closing brace"
                            .to_string(),
                    evidence: line.to_string(),
                });
            }
        }

        // Check for `{+` pattern (less common but possible)
        if let Some(pos) = line.find("{+") {
            let after = &line[pos + 2..];
            if after.trim().len() > 2 && after.chars().next().map_or(false, |c| c.is_whitespace()) {
                issues.push(RobustnessIssue {
                    line: line_num,
                    description:
                        "Embedded diff marker detected - `+` character after opening brace"
                            .to_string(),
                    evidence: line.to_string(),
                });
            }
        }
    }

    issues
}

/// Run the Generic Robustness Probe on content.
///
/// This performs language-agnostic physical integrity checks:
/// 1. Delimiter balance ({}, [], ())
/// 2. Conflict marker detection (<<<<<<<, =======)
/// 3. Embedded diff marker detection (`+` from malformed patches)
///
/// Returns all issues found.
pub fn run_robustness_probe(content: &str) -> Vec<RobustnessIssue> {
    let mut issues = Vec::new();
    issues.extend(check_delimiter_balance(content));
    issues.extend(check_conflict_markers(content));
    issues.extend(check_embedded_diff_markers(content));
    issues
}

/// Validate a single file's content change.
///
/// For supported languages (Rust, Python, JS, TS), uses tree-sitter AST parsing.
/// For unsupported languages, falls back to Generic Robustness Probe (delimiter balance,
/// conflict markers).
pub fn validate_file_change(
    path: &Path,
    before_content: &str,
    after_content: &str,
    patch_byte_range: Option<(usize, usize)>,
) -> ValidationResult {
    let language = SupportedLanguage::from_path(path);

    let (before_errors, after_errors) = if language == SupportedLanguage::Unknown {
        // Phase 2 Audit: Use Generic Robustness Probe for unsupported languages
        let before_issues = run_robustness_probe(before_content);
        let after_issues = run_robustness_probe(after_content);

        // Convert RobustnessIssues to SyntaxErrors
        let before_errors: Vec<SyntaxError> = before_issues
            .into_iter()
            .map(|issue| SyntaxError {
                line: issue.line,
                column: 0,
                start_byte: 0,
                end_byte: 0,
                node_kind: Some("ROBUSTNESS".to_string()),
                context: Some(issue.evidence),
            })
            .collect();

        let after_errors: Vec<SyntaxError> = after_issues
            .into_iter()
            .map(|issue| SyntaxError {
                line: issue.line,
                column: 0,
                start_byte: 0,
                end_byte: 0,
                node_kind: Some("ROBUSTNESS".to_string()),
                context: Some(issue.evidence),
            })
            .collect();

        (before_errors, after_errors)
    } else {
        (
            extract_errors(before_content, language).unwrap_or_default(),
            extract_errors(after_content, language).unwrap_or_default(),
        )
    };

    let error_delta = compute_error_delta(&before_errors, &after_errors, patch_byte_range);

    let is_valid = !error_delta.is_degradation();

    ValidationResult {
        path: path.to_path_buf(),
        language,
        before_errors,
        after_errors,
        is_valid,
        error_delta,
    }
}

/// Simple validation wrapper without patch byte range.
/// Use this when you don't know the exact byte range of the patch.
///
/// Phase 2 Audit: Now validates ALL languages using Generic Robustness Probe as fallback.
pub fn validate_file_change_simple(
    path: &Path,
    before_content: &str,
    after_content: &str,
) -> Option<ValidationResult> {
    // No longer skip unsupported languages - use Generic Robustness Probe
    Some(validate_file_change(
        path,
        before_content,
        after_content,
        None,
    ))
}

/// Generate diagnostic information for a failed validation.
pub fn generate_diagnostics(validation: &ValidationResult) -> Vec<DiagnosticInfo> {
    validation
        .after_errors
        .iter()
        .filter(|e| {
            // Only generate diagnostics for new errors
            !validation
                .before_errors
                .iter()
                .any(|be| be.line == e.line && be.column == e.column)
        })
        .map(|error| {
            let issue = match &error.node_kind {
                Some(kind) if kind == "ERROR" => {
                    format!("Syntax error at Line {}, Column {}", error.line + 1, error.column + 1)
                }
                Some(kind) => {
                    format!(
                        "Missing or unexpected '{}' at Line {}, Column {}",
                        kind,
                        error.line + 1,
                        error.column + 1
                    )
                }
                None => format!("Unknown error at Line {}", error.line + 1),
            };

            let evidence = error
                .context
                .as_deref()
                .unwrap_or("<no context available>");

            DiagnosticInfo {
                path: validation.path.clone(),
                language: Some(validation.language),
                stage: Some("syntax_guard".to_string()),
                tag: Some("APPLY_PATCH_SYNTAX_GUARD".to_string()),
                context: None,
                issue,
                evidence: evidence.to_string(),
                suggestion: "Check the evidence line for a missing delimiter or unexpected token introduced by the patch. If the evidence line begins with a diff prefix, treat this as a split false positive and capture a scenario before retrying."
                    .to_string(),
                next_action: Some("Fix the syntax at the evidence line and rerun apply_patch. If this came from builtin apply_patch, stop retrying and rerun with the apply_patch binary after saving a scenario."
                    .to_string()),
            }
        })
        .collect()
}

/// In-memory representation of a pending file change.
#[derive(Debug, Clone)]
pub enum PendingChange {
    Add {
        path: PathBuf,
        content: String,
    },
    Update {
        path: PathBuf,
        original_content: String,
        new_content: String,
        patch_byte_range: Option<(usize, usize)>,
    },
    Delete {
        path: PathBuf,
        original_content: String,
    },
}

/// Transaction manager for atomic patch application.
#[derive(Debug, Default)]
pub struct PatchTransaction {
    pending_changes: Vec<PendingChange>,
    validations: Vec<ValidationResult>,
}

impl PatchTransaction {
    pub fn new() -> Self {
        Self {
            pending_changes: Vec::new(),
            validations: Vec::new(),
        }
    }

    /// Stage a change for later validation and commit.
    pub fn stage(&mut self, change: PendingChange) {
        self.pending_changes.push(change);
    }

    /// Validate all staged changes.
    /// Returns true if all changes are valid, false otherwise.
    pub fn validate_all(&mut self) -> bool {
        self.validations.clear();

        for change in &self.pending_changes {
            match change {
                PendingChange::Add { path, content } => {
                    // For new files, just check for syntax errors
                    let language = SupportedLanguage::from_path(path);
                    let errors = extract_errors(content, language).unwrap_or_default();

                    let is_valid = errors.is_empty();
                    let error_delta = ErrorDelta {
                        new_errors: errors.len(),
                        fixed_errors: 0,
                        net_change: errors.len() as i64,
                    };

                    self.validations.push(ValidationResult {
                        path: path.clone(),
                        language,
                        before_errors: Vec::new(),
                        after_errors: errors,
                        is_valid,
                        error_delta,
                    });
                }
                PendingChange::Update {
                    path,
                    original_content,
                    new_content,
                    patch_byte_range,
                } => {
                    let result = validate_file_change(
                        path,
                        original_content,
                        new_content,
                        *patch_byte_range,
                    );
                    self.validations.push(result);
                }
                PendingChange::Delete { .. } => {
                    // Deletions don't need syntax validation
                }
            }
        }

        self.validations.iter().all(|v| v.is_valid)
    }

    /// Get validation results for all staged changes.
    pub fn get_validations(&self) -> &[ValidationResult] {
        &self.validations
    }

    /// Get all failed validations with diagnostics.
    pub fn get_failures(&self) -> Vec<(&ValidationResult, Vec<DiagnosticInfo>)> {
        self.validations
            .iter()
            .filter(|v| !v.is_valid)
            .map(|v| (v, generate_diagnostics(v)))
            .collect()
    }

    /// Commit all staged changes to disk.
    /// This should only be called after validate_all() returns true.
    pub fn commit(self) -> anyhow::Result<AffectedPaths> {
        let mut added: Vec<PathBuf> = Vec::new();
        let mut modified: Vec<PathBuf> = Vec::new();
        let mut deleted: Vec<PathBuf> = Vec::new();

        for change in self.pending_changes {
            match change {
                PendingChange::Add { path, content } => {
                    if let Some(parent) = path.parent() {
                        if !parent.as_os_str().is_empty() {
                            std::fs::create_dir_all(parent).with_context(|| {
                                format!(
                                    "Failed to create parent directories for {}",
                                    path.display()
                                )
                            })?;
                        }
                    }
                    std::fs::write(&path, content)
                        .with_context(|| format!("Failed to write file {}", path.display()))?;
                    added.push(path);
                }
                PendingChange::Update {
                    path, new_content, ..
                } => {
                    std::fs::write(&path, new_content)
                        .with_context(|| format!("Failed to write file {}", path.display()))?;
                    modified.push(path);
                }
                PendingChange::Delete {
                    path,
                    original_content: _,
                    ..
                } => {
                    std::fs::remove_file(&path)
                        .with_context(|| format!("Failed to delete file {}", path.display()))?;
                    deleted.push(path);
                }
            }
        }

        Ok(AffectedPaths {
            added,
            modified,
            deleted,
        })
    }

    /// Rollback - in this implementation, we don't write until commit,
    /// so rollback is a no-op (just drop the pending changes).
    pub fn rollback(self) {
        // Pending changes are dropped, nothing written to disk
    }
}

/// Re-export for compatibility
pub use crate::AffectedPaths;

use anyhow::Context;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_supported_language_detection() {
        assert_eq!(
            SupportedLanguage::from_path(Path::new("src/main.rs")),
            SupportedLanguage::Rust
        );
        assert_eq!(
            SupportedLanguage::from_path(Path::new("script.py")),
            SupportedLanguage::Python
        );
        assert_eq!(
            SupportedLanguage::from_path(Path::new("app.js")),
            SupportedLanguage::JavaScript
        );
        assert_eq!(
            SupportedLanguage::from_path(Path::new("component.tsx")),
            SupportedLanguage::TypeScriptTsx
        );
        assert_eq!(
            SupportedLanguage::from_path(Path::new("unknown.xyz")),
            SupportedLanguage::Unknown
        );
    }

    #[test]
    fn test_extract_errors_rust() {
        // Valid Rust code - no errors
        let valid = "fn main() { let x = 1; }";
        let errors = extract_errors(valid, SupportedLanguage::Rust).unwrap();
        assert!(errors.is_empty());

        // Invalid Rust code - unclosed brace
        let invalid = "fn main() { let x = 1; ";
        let errors = extract_errors(invalid, SupportedLanguage::Rust).unwrap();
        assert!(!errors.is_empty());
    }

    #[test]
    fn test_extract_errors_python() {
        // Valid Python code - no errors
        let valid = "def foo():\n    pass";
        let errors = extract_errors(valid, SupportedLanguage::Python).unwrap();
        assert!(errors.is_empty());

        // Invalid Python code - unclosed string
        let invalid = "x = \"unclosed";
        let errors = extract_errors(invalid, SupportedLanguage::Python).unwrap();
        assert!(!errors.is_empty());
    }

    #[test]
    fn test_error_delta_no_change() {
        let before = vec![SyntaxError {
            line: 0,
            column: 0,
            start_byte: 0,
            end_byte: 5,
            node_kind: Some("ERROR".to_string()),
            context: Some("fn main()".to_string()),
        }];
        let after = before.clone();

        let delta = compute_error_delta(&before, &after, None);
        assert_eq!(delta.new_errors, 0);
        assert_eq!(delta.fixed_errors, 0);
        assert_eq!(delta.net_change, 0);
        assert!(!delta.is_degradation());
    }

    #[test]
    fn test_error_delta_new_error() {
        let before: Vec<SyntaxError> = Vec::new();
        let after = vec![SyntaxError {
            line: 0,
            column: 0,
            start_byte: 0,
            end_byte: 5,
            node_kind: Some("ERROR".to_string()),
            context: Some("fn main()".to_string()),
        }];

        let delta = compute_error_delta(&before, &after, None);
        assert_eq!(delta.new_errors, 1);
        assert_eq!(delta.fixed_errors, 0);
        assert_eq!(delta.net_change, 1);
        assert!(delta.is_degradation());
    }

    #[test]
    fn test_error_delta_fixed_error() {
        let before = vec![SyntaxError {
            line: 0,
            column: 0,
            start_byte: 0,
            end_byte: 5,
            node_kind: Some("ERROR".to_string()),
            context: Some("fn main()".to_string()),
        }];
        let after: Vec<SyntaxError> = Vec::new();

        let delta = compute_error_delta(&before, &after, None);
        assert_eq!(delta.new_errors, 0);
        assert_eq!(delta.fixed_errors, 1);
        assert_eq!(delta.net_change, -1);
        assert!(!delta.is_degradation());
    }

    #[test]
    fn test_error_delta_patch_range_filter() {
        // Error outside patch range should not be counted as new
        let before: Vec<SyntaxError> = Vec::new();
        let after = vec![SyntaxError {
            line: 10,
            column: 0,
            start_byte: 100, // Outside patch range
            end_byte: 105,
            node_kind: Some("ERROR".to_string()),
            context: Some("fn other()".to_string()),
        }];

        // Patch range is 0-50, error is at 100
        let delta = compute_error_delta(&before, &after, Some((0, 50)));
        assert_eq!(delta.new_errors, 0); // Filtered out
        assert!(!delta.is_degradation());
    }

    #[test]
    fn test_validate_file_change_valid() {
        let before = "fn main() { let x = 1; }";
        let after = "fn main() { let x = 2; }";

        let result = validate_file_change(
            Path::new("test.rs"),
            before,
            after,
            Some((20, 21)), // Just the "1" -> "2"
        );

        assert!(result.is_valid);
        assert!(result.before_errors.is_empty());
        assert!(result.after_errors.is_empty());
    }

    #[test]
    fn test_validate_file_change_introduces_error() {
        let before = "fn main() { let x = 1; }";
        let after = "fn main() { let x = ; }"; // Missing value

        let result = validate_file_change(
            Path::new("test.rs"),
            before,
            after,
            None, // Don't filter by byte range
        );

        assert!(
            !result.is_valid,
            "Expected invalid result due to new syntax error"
        );
        assert!(
            result.before_errors.is_empty(),
            "Before should have no errors"
        );
        assert!(!result.after_errors.is_empty(), "After should have errors");
        assert!(
            result.error_delta.is_degradation(),
            "Should be a degradation"
        );
    }

    #[test]
    fn test_validate_file_change_pre_existing_error() {
        // Source already has an error
        let before = "fn main() { let x = ; }"; // Pre-existing error
        let after = "fn main() { let y = ; }"; // Still has error, but not new

        let result = validate_file_change(
            Path::new("test.rs"),
            before,
            after,
            Some((15, 16)), // x -> y
        );

        // Should NOT be considered a degradation - error was pre-existing
        assert!(result.is_valid);
    }

    #[test]
    fn test_diagnostic_output() {
        let diag = DiagnosticInfo {
            path: PathBuf::from("src/main.rs"),
            language: Some(SupportedLanguage::Rust),
            stage: Some("syntax_guard".to_string()),
            tag: Some("APPLY_PATCH_SYNTAX_GUARD".to_string()),
            context: Some("function `process_data`".to_string()),
            issue: "Unclosed delimiter '{' at Line 45, Column 12".to_string(),
            evidence: "    if x {".to_string(),
            suggestion: "Check for unclosed delimiters, missing semicolons, or mismatched brackets in the patch."
                .to_string(),
            next_action: Some("Fix the syntax and rerun apply_patch.".to_string()),
        };

        let output = format!("{diag}");
        assert!(output.contains("Syntax Integrity Check Failed"));
        assert!(output.contains("src/main.rs"));
        assert!(output.contains("Unclosed delimiter"));
    }

    // === Generic Robustness Probe Tests (Phase 2 Audit) ===

    #[test]
    fn test_delimiter_balance_valid() {
        let content = r#"fn main() {
    let x = [1, 2, 3];
    let map = {"key": "value"};
}"#;
        let issues = check_delimiter_balance(content);
        assert!(
            issues.is_empty(),
            "Expected no issues for balanced delimiters"
        );
    }

    #[test]
    fn test_delimiter_balance_unclosed_brace() {
        let content = r#"fn main() {
    let x = 1;
    // Missing closing }"#;
        let issues = check_delimiter_balance(content);
        assert!(!issues.is_empty(), "Expected issue for unclosed brace");
        assert!(issues[0].description.contains("Unclosed"));
    }

    #[test]
    fn test_delimiter_balance_mismatched() {
        let content = r#"let arr = [1, 2, 3);"#; // Opens with [, closes with )
        let issues = check_delimiter_balance(content);
        assert!(
            !issues.is_empty(),
            "Expected issue for mismatched delimiters"
        );
    }

    #[test]
    fn test_delimiter_balance_nested() {
        let content = r#"fn outer() {
    fn inner() {
        let x = 1;
    }
}"#;
        let issues = check_delimiter_balance(content);
        assert!(
            issues.is_empty(),
            "Expected no issues for nested balanced blocks"
        );
    }

    #[test]
    fn test_conflict_markers_clean() {
        let content = r#"fn main() {
    println!("Hello");
}"#;
        let issues = check_conflict_markers(content);
        assert!(issues.is_empty(), "Expected no issues for clean content");
    }

    #[test]
    fn test_conflict_markers_detected() {
        let content = r#"fn main() {
<<<<<<< HEAD
    let x = 1;
=======
    let x = 2;
>>>>>>> branch
}"#;
        let issues = check_conflict_markers(content);
        assert!(!issues.is_empty(), "Expected issues for conflict markers");
        assert!(issues.iter().any(|i| i.description.contains("conflict")));
    }

    #[test]
    fn test_robustness_probe_all_checks() {
        // Content with both delimiter issue and conflict marker
        let content = r#"fn main() {
<<<<<<< HEAD
    let x = [1, 2;
}"#;
        let issues = run_robustness_probe(content);
        assert!(issues.len() >= 2, "Expected multiple issues from probe");
    }

    #[test]
    fn test_validate_unsupported_language_with_probe() {
        // C# is not in SupportedLanguage, but should still get probe validation
        let before = r#"class Program {
    static void Main() {
        Console.WriteLine("Hello");
    }
}"#;
        let after = r#"class Program {
    static void Main() {
        Console.WriteLine("Hello");
    // Missing closing }"#;

        let result = validate_file_change(Path::new("Program.cs"), before, after, None);

        // Should detect unclosed brace via Generic Robustness Probe
        assert!(!result.is_valid, "Expected invalid due to unclosed brace");
        assert!(!result.after_errors.is_empty(), "Expected errors in after");
    }

    #[test]
    fn test_validate_unsupported_language_balanced() {
        // Valid C-like code with balanced delimiters
        let before = r#"int main() { return 0; }"#;
        let after = r#"int main() { return 1; }"#;

        let result = validate_file_change(Path::new("main.c"), before, after, None);

        // Should pass probe validation
        assert!(result.is_valid, "Expected valid for balanced code");
    }

    // === Corrupted Patch Detection Tests ===
    // These test the scenario where patches with malformed diff format
    // cause `+` characters to be embedded in source code

    #[test]
    fn test_detect_embedded_plus_characters() {
        // Simulates what happens when a malformed patch leaves `+` in source
        let before = r#"fn rebuild(&mut self) {
    self.clear();
    for doc in docs {
        self.process(doc);
    }
}"#;

        // After a BAD patch that squashed lines and kept `+` prefixes
        let corrupted_after = r#"fn rebuild(&mut self) {
    self.clear();+    for doc in docs {+        self.process(doc);+    }
}"#;

        let result = validate_file_change(Path::new("rebuild.rs"), before, corrupted_after, None);

        // The corrupted code should be detected as invalid
        // Either tree-sitter catches it or the robustness probe does
        assert!(
            !result.is_valid,
            "Expected invalid for corrupted code with embedded +"
        );
    }

    #[test]
    fn test_detect_squashed_lines_in_unknown_language() {
        // Test for a language without tree-sitter support
        let before = r#"pub fn main() {
    let x = 1;
    let y = 2;
}"#;

        // Corrupted output with squashed lines
        let corrupted_after = r#"pub fn main() {+    let x = 1;+    let y = 2;+}"#;

        let result = validate_file_change(
            Path::new("main.xyz"), // Unknown extension
            before,
            corrupted_after,
            None,
        );

        // Generic Robustness Probe should catch unclosed delimiters
        assert!(
            !result.is_valid,
            "Expected invalid for corrupted code in unknown language"
        );
    }

    #[test]
    fn test_valid_multiline_change_accepted() {
        // Ensure valid multi-line changes are still accepted
        let before = r#"fn process() {
    let x = 1;
    let y = 2;
}"#;

        let after = r#"fn process() {
    let x = 10;
    let y = 20;
}"#;

        let result = validate_file_change(Path::new("process.rs"), before, after, None);

        assert!(
            result.is_valid,
            "Expected valid for legitimate multi-line change"
        );
    }
}

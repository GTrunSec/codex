/// Attempt to find the sequence of `pattern` lines within `lines` beginning at or after `start`.
/// Returns the starting index of the match or `None` if not found. Matches are attempted with
/// decreasing strictness: exact match, then ignoring trailing whitespace, then ignoring leading
/// and trailing whitespace. When `eof` is true, we first try starting at the end-of-file (so that
/// patterns intended to match file endings are applied at the end), and fall back to searching
/// from `start` if needed.
///
/// Special cases handled defensively:
///  • Empty `pattern` → returns `Some(start)` (no-op match)
///  • `pattern.len() > lines.len()` → returns `None` (cannot match, avoids
///    out‑of‑bounds panic that occurred pre‑2025‑04‑12)
pub(crate) fn seek_sequence(
    lines: &[String],
    pattern: &[String],
    start: usize,
    eof: bool,
) -> Option<usize> {
    if pattern.is_empty() {
        return Some(start);
    }

    // When the pattern is longer than the available input there is no possible
    // match. Early‑return to avoid the out‑of‑bounds slice that would occur in
    // the search loops below (previously caused a panic when
    // `pattern.len() > lines.len()`).
    if pattern.len() > lines.len() {
        return None;
    }
    let search_start = if eof && lines.len() >= pattern.len() {
        lines.len() - pattern.len()
    } else {
        start
    };
    // Exact match first.
    for i in search_start..=lines.len().saturating_sub(pattern.len()) {
        if lines[i..i + pattern.len()] == *pattern {
            return Some(i);
        }
    }
    // Then rstrip match.
    for i in search_start..=lines.len().saturating_sub(pattern.len()) {
        let mut ok = true;
        for (p_idx, pat) in pattern.iter().enumerate() {
            if lines[i + p_idx].trim_end() != pat.trim_end() {
                ok = false;
                break;
            }
        }
        if ok {
            return Some(i);
        }
    }
    // Finally, trim both sides to allow more lenience.
    for i in search_start..=lines.len().saturating_sub(pattern.len()) {
        let mut ok = true;
        for (p_idx, pat) in pattern.iter().enumerate() {
            if lines[i + p_idx].trim() != pat.trim() {
                ok = false;
                break;
            }
        }
        if ok {
            return Some(i);
        }
    }

    // ------------------------------------------------------------------
    // Final, most permissive pass – attempt to match after *normalising*
    // common Unicode punctuation to their ASCII equivalents so that diffs
    // authored with plain ASCII characters can still be applied to source
    // files that contain typographic dashes / quotes, etc.  This mirrors the
    // fuzzy behaviour of `git apply` which ignores minor byte-level
    // differences when locating context lines.
    // ------------------------------------------------------------------

    fn normalise(s: &str) -> String {
        s.trim()
            .chars()
            .map(|c| match c {
                // Various dash / hyphen code-points → ASCII '-'
                '\u{2010}' | '\u{2011}' | '\u{2012}' | '\u{2013}' | '\u{2014}' | '\u{2015}'
                | '\u{2212}' => '-',
                // Fancy single quotes → '\''
                '\u{2018}' | '\u{2019}' | '\u{201A}' | '\u{201B}' => '\'',
                // Fancy double quotes → '"'
                '\u{201C}' | '\u{201D}' | '\u{201E}' | '\u{201F}' => '"',
                // Non-breaking space and other odd spaces → normal space
                '\u{00A0}' | '\u{2002}' | '\u{2003}' | '\u{2004}' | '\u{2005}' | '\u{2006}'
                | '\u{2007}' | '\u{2008}' | '\u{2009}' | '\u{200A}' | '\u{202F}' | '\u{205F}'
                | '\u{3000}' => ' ',
                other => other,
            })
            .collect::<String>()
    }

    for i in search_start..=lines.len().saturating_sub(pattern.len()) {
        let mut ok = true;
        for (p_idx, pat) in pattern.iter().enumerate() {
            if normalise(&lines[i + p_idx]) != normalise(pat) {
                ok = false;
                break;
            }
        }
        if ok {
            return Some(i);
        }
    }

    // ------------------------------------------------------------------
    // Pass 5: Token-level match (whitespace-agnostic)
    // Normalizes all whitespace sequences to single spaces, then compares
    // token sequences. This handles cases like "let x=1" matching
    // "let  x  =  1" or mixed tabs/spaces.
    // ------------------------------------------------------------------
    fn normalize_tokens(s: &str) -> String {
        s.split_whitespace().collect::<Vec<_>>().join(" ")
    }

    for i in search_start..=lines.len().saturating_sub(pattern.len()) {
        let mut ok = true;
        for (p_idx, pat) in pattern.iter().enumerate() {
            if normalize_tokens(&lines[i + p_idx]) != normalize_tokens(pat) {
                ok = false;
                break;
            }
        }
        if ok {
            return Some(i);
        }
    }

    // ------------------------------------------------------------------
    // Pass 6: Greedy Blank Line Tolerance (Dynamic)
    //
    // IMPROVEMENT: Treats blank lines as "transparent layer" - no limit on skips.
    // As long as non-blank Token sequences match, any number of blank lines
    // in between are ignored. This handles LLM hallucinations that may
    // insert or omit multiple blank lines.
    // ------------------------------------------------------------------

    /// Check if a line is blank (empty or whitespace only)
    fn is_blank(line: &str) -> bool {
        line.trim().is_empty()
    }

    /// Count indentation level (spaces and tabs both count as columns)
    fn count_line_indent(line: &str) -> usize {
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

    /// Greedy blank-tolerant matching: blank lines are transparent.
    /// Only non-blank lines need to match in sequence.
    ///
    /// Phase 2 Audit Improvement: Logical Barrier
    /// When we encounter a dedent (indent < base_indent), stop matching.
    /// This prevents merging logically separate blocks.
    fn match_with_greedy_blank_tolerance(
        source: &[String],
        pattern: &[String],
        start: usize,
    ) -> Option<usize> {
        // Filter to non-blank lines for matching
        let non_blank_pattern: Vec<(usize, &String)> = pattern
            .iter()
            .enumerate()
            .filter(|(_, line)| !is_blank(line))
            .collect();

        if non_blank_pattern.is_empty() {
            // Pattern is all blank lines - just match at start
            return Some(start);
        }

        // Determine base indent from the first pattern line
        let base_indent = count_line_indent(non_blank_pattern.first()?.1);

        // Try to find a sequence where non-blank lines match in order
        // Blank lines are completely ignored (no limit)
        'outer: for source_start in start..=source.len().saturating_sub(1) {
            let mut source_idx = source_start;

            for (_, pat_line) in &non_blank_pattern {
                // Skip ALL blank lines in source (greedy - no limit)
                while source_idx < source.len() && is_blank(&source[source_idx]) {
                    source_idx += 1;
                }

                if source_idx >= source.len() {
                    continue 'outer;
                }

                // LOGICAL BARRIER: Check for dedent (scope end)
                // If current line has less indent than base, we've crossed a logical boundary
                let current_indent = count_line_indent(&source[source_idx]);
                if current_indent < base_indent {
                    continue 'outer;  // Stop matching - crossed logical boundary
                }

                // Check if the non-blank lines match (using trim comparison)
                if source[source_idx].trim() != pat_line.trim() {
                    continue 'outer;
                }

                source_idx += 1;
            }

            // Found a match - return the actual start
            let actual_start = find_actual_start_greedy(source, source_start, &non_blank_pattern);
            return Some(actual_start);
        }

        None
    }

    /// Find the actual start position by working backwards from where we started matching
    fn find_actual_start_greedy(
        source: &[String],
        initial_start: usize,
        non_blank_pattern: &[(usize, &String)],
    ) -> usize {
        if non_blank_pattern.is_empty() {
            return initial_start;
        }

        let first_pat = non_blank_pattern[0].1.trim();

        // Scan backwards from initial_start to find where the first non-blank pattern line appears
        for i in (0..=initial_start).rev() {
            if source[i].trim() == first_pat {
                // Check if preceding lines (if any) are blank or match pattern
                let mut valid = true;
                let mut pat_idx = 0;
                for j in i..=initial_start {
                    if j < source.len() {
                        if pat_idx < non_blank_pattern.len()
                            && source[j].trim() == non_blank_pattern[pat_idx].1.trim()
                        {
                            pat_idx += 1;
                        } else if !is_blank(&source[j]) {
                            valid = false;
                            break;
                        }
                    }
                }
                if valid {
                    return i;
                }
            }
        }
        initial_start
    }

    if let Some(idx) = match_with_greedy_blank_tolerance(lines, pattern, search_start) {
        return Some(idx);
    }

    None
}

#[cfg(test)]
mod tests {
    use super::seek_sequence;
    use std::string::ToString;

    fn to_vec(strings: &[&str]) -> Vec<String> {
        strings.iter().map(ToString::to_string).collect()
    }

    #[test]
    fn test_exact_match_finds_sequence() {
        let lines = to_vec(&["foo", "bar", "baz"]);
        let pattern = to_vec(&["bar", "baz"]);
        assert_eq!(seek_sequence(&lines, &pattern, 0, false), Some(1));
    }

    #[test]
    fn test_rstrip_match_ignores_trailing_whitespace() {
        let lines = to_vec(&["foo   ", "bar\t\t"]);
        // Pattern omits trailing whitespace.
        let pattern = to_vec(&["foo", "bar"]);
        assert_eq!(seek_sequence(&lines, &pattern, 0, false), Some(0));
    }

    #[test]
    fn test_trim_match_ignores_leading_and_trailing_whitespace() {
        let lines = to_vec(&["    foo   ", "   bar\t"]);
        // Pattern omits any additional whitespace.
        let pattern = to_vec(&["foo", "bar"]);
        assert_eq!(seek_sequence(&lines, &pattern, 0, false), Some(0));
    }

    #[test]
    fn test_pattern_longer_than_input_returns_none() {
        let lines = to_vec(&["just one line"]);
        let pattern = to_vec(&["too", "many", "lines"]);
        // Should not panic – must return None when pattern cannot possibly fit.
        assert_eq!(seek_sequence(&lines, &pattern, 0, false), None);
    }

    // === Phase 2 Layer 1: Token-level matching tests ===

    #[test]
    fn test_token_match_ignores_all_whitespace() {
        // Source has multiple spaces between tokens
        let lines = to_vec(&["let  x  =  1"]);
        // Pattern has minimal spaces (same token structure)
        let pattern = to_vec(&["let x = 1"]);
        assert_eq!(seek_sequence(&lines, &pattern, 0, false), Some(0));
    }

    #[test]
    fn test_token_match_handles_tabs() {
        // Source uses tabs
        let lines = to_vec(&["let\tx\t=\t1"]);
        // Pattern uses spaces
        let pattern = to_vec(&["let x = 1"]);
        assert_eq!(seek_sequence(&lines, &pattern, 0, false), Some(0));
    }

    #[test]
    fn test_token_match_mixed_whitespace() {
        // Source has mixed tabs and spaces
        let lines = to_vec(&["fn  foo ( ) \t {"]);
        // Pattern normalized (same token structure)
        let pattern = to_vec(&["fn foo ( ) {"]);
        assert_eq!(seek_sequence(&lines, &pattern, 0, false), Some(0));
    }

    #[test]
    fn test_token_match_multiline() {
        // Both have same token structure with different whitespace
        let lines = to_vec(&["let  x = 1", "let  y = 2"]);
        let pattern = to_vec(&["let x = 1", "let y = 2"]);
        assert_eq!(seek_sequence(&lines, &pattern, 0, false), Some(0));
    }

    #[test]
    fn test_token_match_only_after_strict_passes_fail() {
        // Verify that exact match is still preferred
        let lines = to_vec(&["let x = 1"]);
        let pattern = to_vec(&["let x = 1"]);
        // Should match exactly, not via token normalization
        assert_eq!(seek_sequence(&lines, &pattern, 0, false), Some(0));
    }

    // === Phase 2 Layer 2: Blank line tolerance tests ===

    #[test]
    fn test_blank_line_tolerance_single() {
        // Source has one blank line between code lines
        let lines = to_vec(&["foo", "", "bar"]);
        // Pattern has no blank line
        let pattern = to_vec(&["foo", "bar"]);
        assert_eq!(seek_sequence(&lines, &pattern, 0, false), Some(0));
    }

    #[test]
    fn test_blank_line_tolerance_multiple() {
        // Source has two blank lines (within limit)
        let lines = to_vec(&["foo", "", "", "bar"]);
        // Pattern has no blank lines
        let pattern = to_vec(&["foo", "bar"]);
        assert_eq!(seek_sequence(&lines, &pattern, 0, false), Some(0));
    }

    #[test]
    fn test_blank_line_tolerance_at_end() {
        // Source has blank line at end
        let lines = to_vec(&["foo", "bar", ""]);
        // Pattern has no trailing blank
        let pattern = to_vec(&["foo", "bar"]);
        assert_eq!(seek_sequence(&lines, &pattern, 0, false), Some(0));
    }

    #[test]
    fn test_blank_line_tolerance_in_pattern() {
        // Source and pattern both have blank line
        let lines = to_vec(&["foo", "", "bar"]);
        let pattern = to_vec(&["foo", "", "bar"]);
        // Should match exactly
        assert_eq!(seek_sequence(&lines, &pattern, 0, false), Some(0));
    }

    #[test]
    fn test_blank_line_tolerance_with_code_context() {
        // Realistic case: function with blank lines
        let lines = to_vec(&["fn main() {", "    let x = 1;", "", "    let y = 2;", "}"]);
        let pattern = to_vec(&["fn main() {", "    let x = 1;", "    let y = 2;", "}"]);
        assert_eq!(seek_sequence(&lines, &pattern, 0, false), Some(0));
    }

    // === Greedy Blank Tolerance Tests (no limit on blank skips) ===

    #[test]
    fn test_greedy_blank_tolerance_many_blanks() {
        // Source has 5 blank lines (exceeds old limit of 2)
        let lines = to_vec(&["foo", "", "", "", "", "", "bar"]);
        // Pattern has no blank lines
        let pattern = to_vec(&["foo", "bar"]);
        assert_eq!(seek_sequence(&lines, &pattern, 0, false), Some(0));
    }

    #[test]
    fn test_greedy_blank_tolerance_llm_hallucination() {
        // Simulate LLM hallucination: many blank lines in source, none in patch
        let lines = to_vec(&[
            "fn process() {",
            "",
            "",
            "",
            "    let x = 1;",
            "",
            "",
            "    let y = 2;",
            "}",
        ]);
        let pattern = to_vec(&["fn process() {", "let x = 1;", "let y = 2;", "}"]);
        assert_eq!(seek_sequence(&lines, &pattern, 0, false), Some(0));
    }

    #[test]
    fn test_greedy_blank_tolerance_preserves_exact_match_priority() {
        // When exact match exists, should still prefer it over fuzzy match
        let lines = to_vec(&["foo", "", "bar"]);
        let pattern = to_vec(&["foo", "", "bar"]);
        // Should match exactly (not through greedy blank tolerance)
        assert_eq!(seek_sequence(&lines, &pattern, 0, false), Some(0));
    }

    #[test]
    fn test_greedy_blank_tolerance_interleaved() {
        // Blank lines in both source and pattern at different positions
        let lines = to_vec(&["a", "", "", "b", "", "c"]);
        let pattern = to_vec(&["a", "b", "c"]);
        assert_eq!(seek_sequence(&lines, &pattern, 0, false), Some(0));
    }

    // === Logical Barrier Tests (Phase 2 Audit) ===

    #[test]
    fn test_logical_barrier_stops_at_dedent() {
        // Two functions separated by dedent
        let lines = to_vec(&[
            "fn outer() {",
            "    fn inner() {",
            "        let x = 1;",
            "    }",
            "",
            "    let y = 2;",  // Back to outer's indent
        ]);
        // Pattern for inner function - should NOT match across the dedent
        let pattern = to_vec(&["fn inner() {", "let y = 2;"]);
        // Should NOT match because there's a dedent between them
        assert_eq!(seek_sequence(&lines, &pattern, 0, false), None);
    }

    #[test]
    fn test_logical_barrier_allows_same_indent() {
        // Lines at same indent level should still match
        let lines = to_vec(&[
            "fn foo() {",
            "",
            "",
            "    let x = 1;",
            "",
            "    let y = 2;",
            "}",
        ]);
        let pattern = to_vec(&["let x = 1;", "let y = 2;"]);
        // Should match - greedy blank tolerance starts at index 1 (blank line)
        // and skips to find "let x = 1" at index 3
        // Both source lines are at indent 4, which is >= base_indent (0 from pattern)
        assert_eq!(seek_sequence(&lines, &pattern, 0, false), Some(1));
    }

    #[test]
    fn test_logical_barrier_at_function_boundary() {
        // Two separate functions
        let lines = to_vec(&[
            "fn first() {",
            "    let x = 1;",
            "}",
            "",
            "fn second() {",
            "    let y = 2;",
            "}",
        ]);
        // Pattern looking for first function body + second function declaration
        // Should NOT match because they're in different scopes
        let pattern = to_vec(&["let x = 1;", "fn second() {"]);
        assert_eq!(seek_sequence(&lines, &pattern, 0, false), None);
    }
}

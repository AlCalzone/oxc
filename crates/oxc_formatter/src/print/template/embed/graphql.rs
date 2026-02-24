use rustc_hash::FxHashMap;

use oxc_ast::ast::*;

use crate::{
    ast_nodes::AstNode,
    external_formatter::EmbeddedIR,
    formatter::{Formatter, format_element::LineMode, prelude::*},
    write,
};

use super::super::{FormatTemplateExpression, FormatTemplateExpressionOptions, TemplateExpression};
use super::write_embedded_ir;

/// Per-quasi metadata extracted during the analysis phase.
struct QuasiInfo<'a> {
    /// The cooked text of the quasi (always `Some` — `None` causes early bail-out).
    text: &'a str,
    /// Whether the quasi contains only whitespace and/or GraphQL comments.
    comments_only: bool,
    /// Prettier's `startsWithBlankLine` — blank line at the beginning of this quasi.
    starts_with_blank_line: bool,
    /// Prettier's `endsWithBlankLine` — blank line at the end of this quasi.
    ends_with_blank_line: bool,
}

/// Format a GraphQL template literal via the Doc→IR path.
///
/// Handles both no-substitution and `${}` templates uniformly.
/// No dedent is applied — the GraphQL parser normalizes indentation.
///
/// Called from both tagged template (`gql`...``) and function call (`graphql(schema, `...`)`) paths.
pub(super) fn format_graphql_doc<'a>(
    quasi: &AstNode<'a, TemplateLiteral<'a>>,
    f: &mut Formatter<'_, 'a>,
) -> bool {
    let quasis = &quasi.quasis;
    let num_quasis = quasis.len();

    // Phase 1: Analyze each quasi (using cooked values, like Prettier).
    let mut infos: Vec<QuasiInfo<'a>> = Vec::with_capacity(num_quasis);
    for (i, quasi_elem) in quasis.iter().enumerate() {
        // Use cooked value instead of raw. Bail out if cooked is None
        // (invalid escape sequence — matches Prettier's behavior for invalid.js).
        let Some(cooked) = quasi_elem.value.cooked.as_ref() else {
            return false;
        };
        let text = cooked.as_str();

        let is_last = i == num_quasis - 1;

        let lines: Vec<&str> = text.split('\n').collect();

        // Bail out if interpolation occurs within a GraphQL comment.
        // Prettier (graphql.js:37): `if (!isLast && /#[^\n\r]*$/.test(lines[numLines-1]))`
        // Must use `lines.last()` (from split), not `text.lines().next_back()`,
        // because `str::lines()` strips trailing empty lines — causing false
        // positives when text ends with `\n` (e.g. `"\n# comment\n"`).
        if !is_last
            && let Some(last_line) = lines.last()
            && last_line.contains('#')
        {
            return false;
        }
        let num_lines = lines.len();

        // Detect blank lines around expressions (Prettier graphql.js:25-30).
        let starts_with_blank_line =
            num_lines > 2 && lines[0].trim().is_empty() && lines[1].trim().is_empty();
        let ends_with_blank_line = num_lines > 2
            && lines[num_lines - 1].trim().is_empty()
            && lines[num_lines - 2].trim().is_empty();

        let comments_only = is_graphql_comments_and_whitespace_only(text);

        infos.push(QuasiInfo { text, comments_only, starts_with_blank_line, ends_with_blank_line });
    }

    // Phase 2: Collect non-skip texts for batch formatting.
    // Only send texts that actually need formatting to JS.
    let mut texts_to_format: Vec<&str> = Vec::new();
    let mut format_index_map: Vec<Option<usize>> = Vec::with_capacity(num_quasis);
    for info in &infos {
        if info.comments_only {
            format_index_map.push(None);
        } else {
            format_index_map.push(Some(texts_to_format.len()));
            texts_to_format.push(info.text);
        }
    }

    // Batch call: send only non-skip texts, get IRs back.
    let all_irs = if texts_to_format.is_empty() {
        Vec::new()
    } else {
        let Some(Ok(irs)) = f
            .context()
            .external_callbacks()
            .format_embedded_doc("tagged-graphql", &texts_to_format)
        else {
            return false;
        };
        irs
    };

    // Phase 3: Build ir_parts by mapping formatted results back to original indices.
    let mut ir_parts: Vec<Option<Vec<EmbeddedIR>>> = Vec::with_capacity(num_quasis);
    for (i, info) in infos.iter().enumerate() {
        if let Some(fmt_idx) = format_index_map[i] {
            // Formatted by external callback
            ir_parts.push(all_irs.get(fmt_idx).cloned());
        } else if info.comments_only {
            // Build IR for comment-only quasis in Rust (Prettier's printGraphqlComments)
            let comment_ir = build_graphql_comment_ir(info.text);
            ir_parts.push(comment_ir);
        } else {
            ir_parts.push(None);
        }
    }

    // Collect expressions via AstNode-aware iterator
    // (FormatTemplateExpression needs AstNode-wrapped expressions)
    let expressions: Vec<_> = quasi.expressions().iter().collect();

    // Early return for empty/whitespace-only templates with no expressions.
    // Prettier outputs e.g. `gql``; ` for these cases.
    // `block_indent` requires at least one element, so we must avoid it here.
    if expressions.is_empty() && ir_parts.iter().all(Option::is_none) {
        write!(f, ["``"]);
        return true;
    }

    // Phase 4: Write the template structure (mirroring Prettier's graphql.js:67):
    //   `` ` `` + indent(hardline + join(hardline, parts)) + hardline + `` ` ``
    let format_content = format_with(|f: &mut Formatter<'_, 'a>| {
        let mut group_id_map = FxHashMap::default();
        let mut has_prev_part = false;

        for (i, maybe_ir) in ir_parts.iter().enumerate() {
            let is_first = i == 0;
            let is_last = i == num_quasis - 1;

            if let Some(ir) = maybe_ir {
                // Insert blank line before content if startsWithBlankLine
                if !is_first && infos[i].starts_with_blank_line {
                    if has_prev_part {
                        write!(f, [empty_line()]);
                    }
                } else if has_prev_part {
                    write!(f, [hard_line_break()]);
                }
                write_embedded_ir(ir, f, &mut group_id_map);
                has_prev_part = true;
            } else if !is_first && !is_last && infos[i].starts_with_blank_line {
                // Prettier (graphql.js:58-60): when doc is null but startsWithBlankLine,
                // still push an empty string (which becomes a blank line in join).
                if has_prev_part {
                    write!(f, [empty_line()]);
                }
            }

            // Insert blank line after content if endsWithBlankLine
            if !is_last {
                if infos[i].ends_with_blank_line && has_prev_part {
                    write!(f, [empty_line()]);
                    has_prev_part = false; // Next part won't add another separator
                }

                if let Some(expr) = expressions.get(i) {
                    if has_prev_part {
                        write!(f, [hard_line_break()]);
                    }
                    let te = TemplateExpression::Expression(expr);
                    FormatTemplateExpression::new(&te, FormatTemplateExpressionOptions::default())
                        .fmt(f);
                    has_prev_part = true;
                }
            }
        }
    });

    write!(f, ["`", block_indent(&format_content), "`"]);
    true
}

/// Check if every line in the text is whitespace-only or a GraphQL comment (`# ...`).
fn is_graphql_comments_and_whitespace_only(text: &str) -> bool {
    text.split('\n').all(|line| {
        let trimmed = line.trim();
        trimmed.is_empty() || trimmed.starts_with('#')
    })
}

/// Build IR for a comment-only quasi (Prettier's `printGraphqlComments`).
///
/// Extracts comment lines, joins with hardline, and preserves blank lines
/// between comment groups.
fn build_graphql_comment_ir(text: &str) -> Option<Vec<EmbeddedIR>> {
    let lines: Vec<&str> = text.split('\n').map(str::trim).collect();
    let mut parts: Vec<EmbeddedIR> = Vec::new();
    let mut seen_comment = false;

    for (i, line) in lines.iter().enumerate() {
        if line.is_empty() {
            continue;
        }

        if i > 0 && lines[i - 1].is_empty() && seen_comment {
            // Blank line before this comment group → emit empty line + text
            parts.push(EmbeddedIR::Line(LineMode::Empty));
            parts.push(EmbeddedIR::ExpandParent);
            parts.push(EmbeddedIR::Text((*line).to_string()));
        } else {
            if seen_comment {
                parts.push(EmbeddedIR::Line(LineMode::Hard));
                parts.push(EmbeddedIR::ExpandParent);
            }
            parts.push(EmbeddedIR::Text((*line).to_string()));
        }

        seen_comment = true;
    }

    if parts.is_empty() { None } else { Some(parts) }
}

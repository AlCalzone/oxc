mod graphql;

use std::num::NonZeroU8;

use rustc_hash::FxHashMap;

use oxc_allocator::{Allocator, StringBuilder};
use oxc_ast::ast::*;
use oxc_syntax::line_terminator::LineTerminatorSplitter;

use crate::{
    ast_nodes::{AstNode, AstNodes},
    external_formatter::EmbeddedIR,
    formatter::{
        FormatElement, Formatter, GroupId,
        format_element::{TextWidth, tag},
        prelude::*,
    },
    write,
};

// ==================== Dispatch ====================

/// Try to format a tagged template with the embedded formatter if supported.
/// Returns `true` if formatting was performed, `false` if not applicable.
pub(super) fn try_format_embedded_template<'a>(
    tagged: &AstNode<'a, TaggedTemplateExpression<'a>>,
    f: &mut Formatter<'_, 'a>,
) -> bool {
    match get_tag_name(&tagged.tag) {
        Some("css" | "styled") => try_embed_css(tagged, f),
        Some("gql" | "graphql") => graphql::format_graphql_doc(tagged.quasi(), f),
        Some("html") => try_embed_html(tagged, f),
        Some("md" | "markdown") => try_embed_markdown(tagged, f),
        _ => false,
    }
}

/// Try to format a template literal inside css prop or styled-jsx with the embedded formatter.
/// Returns `true` if formatting was attempted, `false` if not applicable.
pub(super) fn try_format_css_template<'a>(
    template_literal: &AstNode<'a, TemplateLiteral<'a>>,
    f: &mut Formatter<'_, 'a>,
) -> bool {
    // TODO: Support expressions in the template
    if !template_literal.is_no_substitution_template() {
        return false;
    }

    if !is_in_css_jsx(template_literal) {
        return false;
    }

    let template_content = template_literal.quasis()[0].value.raw.as_str();
    format_embedded_template(f, "styled-jsx", template_content)
}

/// Try to format a template literal inside Angular @Component's template/styles property.
/// Returns `true` if formatting was performed, `false` if not applicable.
pub(super) fn try_format_angular_component<'a>(
    template_literal: &AstNode<'a, TemplateLiteral<'a>>,
    f: &mut Formatter<'_, 'a>,
) -> bool {
    // TODO: Support expressions in the template
    if !template_literal.is_no_substitution_template() {
        return false;
    }

    let Some(language) = get_angular_component_language(template_literal) else {
        return false;
    };

    let template_content = template_literal.quasis()[0].value.raw.as_str();
    format_embedded_template(f, language, template_content)
}

// ==================== Per-language entry points ====================

fn try_embed_css<'a>(
    tagged: &AstNode<'a, TaggedTemplateExpression<'a>>,
    f: &mut Formatter<'_, 'a>,
) -> bool {
    // TODO(PR-4): Remove this check and use placeholder approach for expressions
    if !tagged.quasi.is_no_substitution_template() {
        return false;
    }
    let template_content = tagged.quasi.quasis[0].value.raw.as_str();
    format_embedded_template(f, "tagged-css", template_content)
}

fn try_embed_html<'a>(
    tagged: &AstNode<'a, TaggedTemplateExpression<'a>>,
    f: &mut Formatter<'_, 'a>,
) -> bool {
    // TODO(PR-5): Remove this check and use placeholder approach for expressions
    if !tagged.quasi.is_no_substitution_template() {
        return false;
    }
    let template_content = tagged.quasi.quasis[0].value.raw.as_str();
    format_embedded_template(f, "tagged-html", template_content)
}

fn try_embed_markdown<'a>(
    tagged: &AstNode<'a, TaggedTemplateExpression<'a>>,
    f: &mut Formatter<'_, 'a>,
) -> bool {
    // Markdown never supports expressions (Prettier doesn't either)
    if !tagged.quasi.is_no_substitution_template() {
        return false;
    }
    let template_content = tagged.quasi.quasis[0].value.raw.as_str();
    format_embedded_template(f, "tagged-markdown", template_content)
}

// ==================== Shared utilities ====================

fn get_tag_name<'a>(expr: &'a Expression<'a>) -> Option<&'a str> {
    let expr = expr.get_inner_expression();
    match expr {
        Expression::Identifier(ident) => Some(ident.name.as_str()),
        Expression::StaticMemberExpression(member) => get_tag_name(&member.object),
        Expression::ComputedMemberExpression(exp) => get_tag_name(&exp.object),
        Expression::CallExpression(call) => get_tag_name(&call.callee),
        _ => None,
    }
}

/// Format embedded language content inside a template literal using the string path.
///
/// This is the shared formatting logic for no-substitution templates:
/// dedent → external formatter (Prettier) → reconstruct template structure.
fn format_embedded_template<'a>(
    f: &mut Formatter<'_, 'a>,
    language: &str,
    template_content: &str,
) -> bool {
    if template_content.trim().is_empty() {
        write!(f, ["``"]);
        return true;
    }

    let template_content = dedent(template_content, f.context().allocator());

    let Some(Ok(formatted)) =
        f.context().external_callbacks().format_embedded(language, template_content)
    else {
        return false;
    };

    let format_content = format_with(|f: &mut Formatter<'_, 'a>| {
        let content = f.context().allocator().alloc_str(&formatted);
        for line in LineTerminatorSplitter::new(content) {
            if line.is_empty() {
                write!(f, [empty_line()]);
            } else {
                write!(f, [text(line), hard_line_break()]);
            }
        }
    });

    write!(f, ["`", block_indent(&format_content), "`"]);
    true
}

/// Strip the common leading indentation from all non-empty lines in `text`.
pub(super) fn dedent<'a>(text: &'a str, allocator: &'a Allocator) -> &'a str {
    let min_indent = text
        .split('\n')
        .filter(|line| !line.trim_ascii_start().is_empty())
        .map(|line| line.bytes().take_while(u8::is_ascii_whitespace).count())
        .min()
        .unwrap_or(0);

    if min_indent == 0 {
        return text;
    }

    let mut result = StringBuilder::with_capacity_in(text.len(), allocator);
    for (i, line) in text.split('\n').enumerate() {
        if i > 0 {
            result.push('\n');
        }
        let strip = line.bytes().take_while(u8::is_ascii_whitespace).count().min(min_indent);
        result.push_str(&line[strip..]);
    }

    result.into_str()
}

/// Write a sequence of `EmbeddedIR` elements into the formatter buffer,
/// converting each to the corresponding `FormatElement<'a>`.
pub(super) fn write_embedded_ir(
    ir: &[EmbeddedIR],
    f: &mut Formatter<'_, '_>,
    group_id_map: &mut FxHashMap<u32, GroupId>,
) {
    let indent_width = f.options().indent_width;
    for item in ir {
        match item {
            EmbeddedIR::Space => f.write_element(FormatElement::Space),
            EmbeddedIR::HardSpace => f.write_element(FormatElement::HardSpace),
            EmbeddedIR::Line(mode) => f.write_element(FormatElement::Line(*mode)),
            EmbeddedIR::ExpandParent => f.write_element(FormatElement::ExpandParent),
            EmbeddedIR::Text(s) => {
                // Escape template characters to avoid breaking template literal syntax.
                // Matches Prettier's `uncookTemplateElementValue`:
                //   cookedValue.replaceAll(/([\\`]|\$\{)/g, "\\$1")
                let escaped = escape_template_characters(s);
                let text = f.allocator().alloc_str(&escaped);
                let width = TextWidth::from_text(text, indent_width);
                f.write_element(FormatElement::Text { text, width });
            }
            EmbeddedIR::LineSuffixBoundary => {
                f.write_element(FormatElement::LineSuffixBoundary);
            }
            EmbeddedIR::StartIndent => {
                f.write_element(FormatElement::Tag(tag::Tag::StartIndent));
            }
            EmbeddedIR::EndIndent => {
                f.write_element(FormatElement::Tag(tag::Tag::EndIndent));
            }
            EmbeddedIR::StartAlign(n) => {
                if let Some(nz) = NonZeroU8::new(*n) {
                    f.write_element(FormatElement::Tag(tag::Tag::StartAlign(tag::Align(nz))));
                }
            }
            EmbeddedIR::EndAlign => {
                f.write_element(FormatElement::Tag(tag::Tag::EndAlign));
            }
            EmbeddedIR::StartDedent { to_root } => {
                let mode = if *to_root { tag::DedentMode::Root } else { tag::DedentMode::Level };
                f.write_element(FormatElement::Tag(tag::Tag::StartDedent(mode)));
            }
            EmbeddedIR::EndDedent { to_root } => {
                let mode = if *to_root { tag::DedentMode::Root } else { tag::DedentMode::Level };
                f.write_element(FormatElement::Tag(tag::Tag::EndDedent(mode)));
            }
            EmbeddedIR::StartGroup { id, should_break } => {
                let gid = id.map(|n| resolve_group_id(n, group_id_map, f));
                let mode =
                    if *should_break { tag::GroupMode::Expand } else { tag::GroupMode::Flat };
                f.write_element(FormatElement::Tag(tag::Tag::StartGroup(
                    tag::Group::new().with_id(gid).with_mode(mode),
                )));
            }
            EmbeddedIR::EndGroup => {
                f.write_element(FormatElement::Tag(tag::Tag::EndGroup));
            }
            EmbeddedIR::StartConditionalContent { mode, group_id } => {
                let gid = group_id.map(|n| resolve_group_id(n, group_id_map, f));
                f.write_element(FormatElement::Tag(tag::Tag::StartConditionalContent(
                    tag::Condition::new(*mode).with_group_id(gid),
                )));
            }
            EmbeddedIR::EndConditionalContent => {
                f.write_element(FormatElement::Tag(tag::Tag::EndConditionalContent));
            }
            EmbeddedIR::StartIndentIfGroupBreaks(id) => {
                let gid = resolve_group_id(*id, group_id_map, f);
                f.write_element(FormatElement::Tag(tag::Tag::StartIndentIfGroupBreaks(gid)));
            }
            EmbeddedIR::EndIndentIfGroupBreaks(id) => {
                let gid = resolve_group_id(*id, group_id_map, f);
                f.write_element(FormatElement::Tag(tag::Tag::EndIndentIfGroupBreaks(gid)));
            }
            EmbeddedIR::StartFill => {
                f.write_element(FormatElement::Tag(tag::Tag::StartFill));
            }
            EmbeddedIR::EndFill => {
                f.write_element(FormatElement::Tag(tag::Tag::EndFill));
            }
            EmbeddedIR::StartEntry => {
                f.write_element(FormatElement::Tag(tag::Tag::StartEntry));
            }
            EmbeddedIR::EndEntry => {
                f.write_element(FormatElement::Tag(tag::Tag::EndEntry));
            }
            EmbeddedIR::StartLineSuffix => {
                f.write_element(FormatElement::Tag(tag::Tag::StartLineSuffix));
            }
            EmbeddedIR::EndLineSuffix => {
                f.write_element(FormatElement::Tag(tag::Tag::EndLineSuffix));
            }
        }
    }
}

/// Look up or create a `GroupId` for the given numeric ID.
pub(super) fn resolve_group_id(
    id: u32,
    map: &mut FxHashMap<u32, GroupId>,
    f: &Formatter<'_, '_>,
) -> GroupId {
    *map.entry(id).or_insert_with(|| f.group_id("embedded"))
}

/// Escape characters that would break template literal syntax.
///
/// Equivalent to Prettier's `uncookTemplateElementValue`:
/// `cookedValue.replaceAll(/([\\`]|\$\{)/g, "\\$1")`
fn escape_template_characters(s: &str) -> String {
    let mut result = String::with_capacity(s.len());
    let bytes = s.as_bytes();
    let len = bytes.len();
    let mut i = 0;
    while i < len {
        let ch = bytes[i];
        if ch == b'\\' || ch == b'`' {
            result.push('\\');
            result.push(ch as char);
        } else if ch == b'$' && i + 1 < len && bytes[i + 1] == b'{' {
            result.push_str("\\${");
            i += 1; // skip '{'
        } else {
            result.push(ch as char);
        }
        i += 1;
    }
    result
}

// ==================== Detection helpers ====================

/// Check if the template literal is inside a `css` prop or `<style jsx>` element.
fn is_in_css_jsx<'a>(node: &AstNode<'a, TemplateLiteral<'a>>) -> bool {
    let AstNodes::JSXExpressionContainer(container) = node.parent() else {
        return false;
    };

    match container.parent() {
        AstNodes::JSXAttribute(attribute) => {
            if let JSXAttributeName::Identifier(ident) = &attribute.name
                && ident.name == "css"
            {
                return true;
            }
        }
        AstNodes::JSXElement(element) => {
            if let JSXElementName::Identifier(ident) = &element.opening_element.name
                && ident.name == "style"
                && element.opening_element.attributes.iter().any(|attr| {
                    matches!(attr.as_attribute().and_then(|a| a.name.as_identifier()), Some(name) if name.name == "jsx")
                })
            {
                return true;
            }
        }
        _ => {}
    }
    false
}

/// Detect Angular `@Component({ template: \`...\`, styles: \`...\` })`.
fn get_angular_component_language(node: &AstNode<'_, TemplateLiteral<'_>>) -> Option<&'static str> {
    let prop = match node.parent() {
        AstNodes::ObjectProperty(prop) => prop,
        AstNodes::ArrayExpression(arr) => {
            let AstNodes::ObjectProperty(prop) = arr.parent() else {
                return None;
            };
            prop
        }
        _ => return None,
    };

    if prop.computed {
        return None;
    }
    let PropertyKey::StaticIdentifier(key) = &prop.key else {
        return None;
    };

    let AstNodes::ObjectExpression(obj) = prop.parent() else {
        return None;
    };
    let AstNodes::CallExpression(call) = obj.parent() else {
        return None;
    };
    let Expression::Identifier(ident) = &call.callee else {
        return None;
    };
    if ident.name.as_str() != "Component" {
        return None;
    }
    if !matches!(call.parent(), AstNodes::Decorator(_)) {
        return None;
    }

    match key.name.as_str() {
        "template" => Some("angular-template"),
        "styles" => Some("angular-styles"),
        _ => None,
    }
}

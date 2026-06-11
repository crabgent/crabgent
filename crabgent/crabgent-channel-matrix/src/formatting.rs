//! Matrix-specific output-format hint for `KernelChannelInbox::with_formatting_hint`.
//!
//! Source: Matrix client-server specification, `m.room.message` event with
//! `format = "org.matrix.custom.html"` and `formatted_body`:
//! <https://spec.matrix.org/latest/client-server-api/#mroommessage>.
//! The const is trusted crate-internal content and is not run through
//! `sanitize_for_prompt` (see the `crabgent-formatting-hints` skill,
//! section 3).

/// System-prompt suffix that instructs the LLM to produce HTML in the
/// `org.matrix.custom.html` subset rather than Markdown when responding
/// inside a Matrix room. Wired up via
/// [`crabgent_channel::KernelChannelInbox::with_formatting_hint`].
pub const MATRIX_FORMATTING_HINT: &str = "\
<output_format>
You are responding in a Matrix room. Format output as HTML using the org.matrix.custom.html format.
Plain Markdown is NOT reliably rendered by Matrix clients.
The channel adapter remains the wire-format source of truth and normalizes plain text or basic Markdown fallbacks; use these rules for best Matrix rendering.

Chat layout rules:
- Keep each paragraph short: at most 2 sentences or 3 short lines.
- Split findings into compact sections with blank lines. Prefer bullets for multiple concrete items.
- Do not send one dense diagnostic paragraph with many paths, ids, and key=value pairs.
- Use inline <code> only for short exact tokens, file names, commands, or ids.
- Do not wrap whole phrases, long key=value sequences, or comma-separated target lists in inline <code>; use plain text or a short <pre> block with one item per line.
- Avoid repeating local display names or Matrix localparts inside product/repo identifiers when a neutral label is clear enough; this prevents accidental mention/highlight noise.

Allowed tags (Matrix client-server spec): del, h1, h2, h3, h4, h5, h6,
blockquote, p, a, ul, ol, sup, sub, li, b, i, u, strong, em, s, code,
hr, br, div, table, thead, tbody, tr, th, td, caption, pre, span, img,
details, summary.

Formatting rules:
- Mentions: <a href=\"https://matrix.to/#/@user:home.server\">Name</a>
- Room links: <a href=\"https://matrix.to/#/!roomid:home.server\">#room</a>
  or <a href=\"https://matrix.to/#/#alias:home.server\">#alias</a>
- Colors: use <span data-mx-color=\"#RRGGBB\">text</span> or
  <span data-mx-bg-color=\"#RRGGBB\">text</span>; do not use <font>
- Escape outside tags: & as &amp;, < as &lt;, > as &gt;
- NO <script>, NO <iframe>, NO event-handler attributes (onclick, onerror, etc.)
</output_format>";

#[cfg(test)]
mod tests {
    use super::MATRIX_FORMATTING_HINT;

    #[test]
    fn hint_mentions_org_matrix_custom_html() {
        assert!(
            MATRIX_FORMATTING_HINT.contains("org.matrix.custom.html"),
            "MATRIX_FORMATTING_HINT must mention the Matrix HTML format"
        );
    }

    #[test]
    fn hint_names_matrix_html_over_markdown() {
        assert!(
            MATRIX_FORMATTING_HINT.contains("Plain Markdown is NOT reliably rendered"),
            "Matrix hint should discourage plain Markdown"
        );
    }

    #[test]
    fn hint_guides_chat_sized_layout() {
        for rule in [
            "Keep each paragraph short",
            "Split findings into compact sections",
            "Do not send one dense diagnostic paragraph",
            "Prefer bullets",
        ] {
            assert!(
                MATRIX_FORMATTING_HINT.contains(rule),
                "Matrix hint missing chat layout rule: {rule}"
            );
        }
    }

    #[test]
    fn hint_limits_inline_code_noise() {
        for rule in [
            "Use inline <code> only for short exact tokens",
            "Do not wrap whole phrases",
            "long key=value sequences",
            "one item per line",
            "accidental mention/highlight noise",
        ] {
            assert!(
                MATRIX_FORMATTING_HINT.contains(rule),
                "Matrix hint missing inline-code rule: {rule}"
            );
        }
    }

    #[test]
    fn hint_lists_matrix_spec_tags_and_links() {
        for rule in [
            "h1",
            "blockquote",
            "ul",
            "ol",
            "li",
            "code",
            "pre",
            "table",
            "span",
            "details",
            "summary",
            "https://matrix.to/#/@user:home.server",
            "https://matrix.to/#/!roomid:home.server",
        ] {
            assert!(
                MATRIX_FORMATTING_HINT.contains(rule),
                "Matrix hint missing supported tag/rule: {rule}"
            );
        }
    }

    #[test]
    fn hint_uses_span_color_attrs_not_deprecated_font_guidance() {
        assert!(
            MATRIX_FORMATTING_HINT.contains("data-mx-color"),
            "Matrix hint should document data-mx-color"
        );
        assert!(
            MATRIX_FORMATTING_HINT.contains("data-mx-bg-color"),
            "Matrix hint should document data-mx-bg-color"
        );
        assert!(
            !MATRIX_FORMATTING_HINT.contains("font (color"),
            "Matrix hint must not advertise deprecated font color guidance"
        );
    }

    #[test]
    fn hint_documents_matrix_escape_and_security_rules() {
        for rule in [
            "& as &amp;",
            "< as &lt;",
            "> as &gt;",
            "NO <script>",
            "NO <iframe>",
            "NO event-handler attributes",
        ] {
            assert!(
                MATRIX_FORMATTING_HINT.contains(rule),
                "Matrix hint missing rule: {rule}"
            );
        }
    }

    #[test]
    fn hint_wrapped_in_output_format() {
        assert!(
            MATRIX_FORMATTING_HINT.starts_with("<output_format>"),
            "must open with <output_format> tag"
        );
        assert!(
            MATRIX_FORMATTING_HINT.ends_with("</output_format>"),
            "must close with </output_format> tag"
        );
    }

    #[test]
    fn hint_contains_no_control_chars() {
        for c in MATRIX_FORMATTING_HINT.chars() {
            assert!(
                !c.is_control() || c == '\n',
                "disallowed control char in MATRIX_FORMATTING_HINT: U+{:04X}",
                c as u32
            );
        }
    }
}

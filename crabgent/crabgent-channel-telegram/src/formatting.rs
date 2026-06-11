//! Telegram-specific output-format hint for `KernelChannelInbox::with_formatting_hint`.
//!
//! Source: Telegram Bot API formatting options:
//! <https://core.telegram.org/bots/api#html-style>. The const is trusted
//! crate-internal content and is not run through `sanitize_for_prompt`
//! (see the `crabgent-formatting-hints` skill, section 3).

/// System-prompt suffix that instructs the LLM to produce Telegram Bot
/// API HTML (`parse_mode=HTML`) rather than `MarkdownV2` or generic
/// Markdown when responding inside a Telegram chat. Wired up via
/// [`crabgent_channel::KernelChannelInbox::with_formatting_hint`].
pub const TELEGRAM_FORMATTING_HINT: &str = "\
<output_format>
You are responding in Telegram. Use Telegram HTML (parse_mode=HTML), NOT MarkdownV2.
The channel adapter remains the wire-format source of truth and normalizes plain text or basic Markdown fallbacks; use these rules for best Telegram rendering.

Allowed tags: b, strong, i, em, u, ins, s, strike, del,
span class=\"tg-spoiler\", tg-spoiler, a href, tg-emoji,
tg-time, code, pre, pre/code with class=\"language-XYZ\",
blockquote, blockquote expandable.

Rules:
- Escape outside tags AND inside <code>/<pre>: & as &amp;, < as &lt;, > as &gt;
- Links: <a href=\"https://...\">text</a> (http/https, tg://, mailto:)
- Mentions: <a href=\"tg://user?id=USER_ID\">Name</a>
- NO nested same-tag (e.g. <b><b>x</b></b> is invalid)
- NO self-closing tags like <br/> - use plain newline (\n) instead
- NO ## headers (no <h*> tags supported)
- NO list tags (<ul>/<ol>/<li> are not rendered) - use plain \"- \" bullets
- Message length limit: 4096 characters
</output_format>";

#[cfg(test)]
mod tests {
    use super::TELEGRAM_FORMATTING_HINT;

    #[test]
    fn hint_mentions_parse_mode_html() {
        assert!(
            TELEGRAM_FORMATTING_HINT.contains("parse_mode=HTML"),
            "TELEGRAM_FORMATTING_HINT must mention parse_mode=HTML"
        );
    }

    #[test]
    fn hint_points_to_official_telegram_docs() {
        assert!(
            TELEGRAM_FORMATTING_HINT.contains("Telegram HTML"),
            "hint should name Telegram HTML as the output format"
        );
    }

    #[test]
    fn hint_lists_supported_telegram_html_tags() {
        for tag in [
            "b",
            "strong",
            "i",
            "em",
            "u",
            "ins",
            "s",
            "strike",
            "del",
            "tg-spoiler",
            "a href",
            "tg-emoji",
            "tg-time",
            "code",
            "pre",
            "blockquote",
        ] {
            assert!(
                TELEGRAM_FORMATTING_HINT.contains(tag),
                "Telegram hint missing supported tag/rule: {tag}"
            );
        }
    }

    #[test]
    fn hint_documents_telegram_escaping_and_unsupported_structures() {
        for rule in [
            "& as &amp;",
            "< as &lt;",
            "> as &gt;",
            "NO self-closing tags",
            "NO ## headers",
            "NO list tags",
            "4096 characters",
        ] {
            assert!(
                TELEGRAM_FORMATTING_HINT.contains(rule),
                "Telegram hint missing rule: {rule}"
            );
        }
    }

    #[test]
    fn hint_wrapped_in_output_format() {
        assert!(
            TELEGRAM_FORMATTING_HINT.starts_with("<output_format>"),
            "must open with <output_format> tag"
        );
        assert!(
            TELEGRAM_FORMATTING_HINT.ends_with("</output_format>"),
            "must close with </output_format> tag"
        );
    }

    #[test]
    fn hint_contains_no_control_chars() {
        for c in TELEGRAM_FORMATTING_HINT.chars() {
            assert!(
                !c.is_control() || c == '\n',
                "disallowed control char in TELEGRAM_FORMATTING_HINT: U+{:04X}",
                c as u32
            );
        }
    }
}

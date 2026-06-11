//! Slack-specific output-format hint for `KernelChannelInbox::with_formatting_hint`.
//!
//! Source: Slack message formatting docs:
//! <https://docs.slack.dev/messaging/formatting-message-text/>. The const is
//! trusted crate-internal content and is therefore not run through
//! `sanitize_for_prompt` (see the `crabgent-formatting-hints` skill,
//! section 3).

/// System-prompt suffix that instructs the LLM to produce Slack `mrkdwn`
/// rather than generic Markdown when responding inside a Slack channel
/// or thread. Wired up via
/// [`crabgent_channel::KernelChannelInbox::with_formatting_hint`].
pub const SLACK_FORMATTING_HINT: &str = "\
<output_format>
You are responding in Slack. ALL text you produce MUST use Slack mrkdwn, NOT Markdown.
The channel adapter remains the wire-format source of truth and normalizes plain text or basic Markdown fallbacks; use these rules for best Slack rendering.

Slack mrkdwn rules:
- Bold: *text* (single asterisk). NEVER **text**.
- Italic: _text_ (underscores).
- Code inline: `text` (backticks).
- Code block: triple backticks.
- Strikethrough: ~text~.
- Lists: Slack has no real list syntax; mimic lists with separate lines
  starting with `- ` or `1. `.
- Links: <url|label>.
- Slack channel links: <#CHANNEL_ID> renders the correct channel name. If a correct channel name is available from Slack context or tool results, <#CHANNEL_ID|channel-name> is also valid. NEVER guess channel names; prefer <#CHANNEL_ID> when only the ID is known.
- NO ## headers. Slack does not render them. Use *bold text* on its own line instead.
- NO horizontal rules (---). Use a blank line to separate sections.
- NO tables (| syntax does not render). Use plain text with line breaks instead.
- NO HTML tags.
- Slack renders :emoji_name: as emoji.
Keep messages concise. Slack is a chat context, NOT a document.
- Never include raw Slack user IDs (for example U123ABC).
- To refer to a person, use either a proper mention (`<@USER_ID>`) or a resolved user/display name.
</output_format>";

#[cfg(test)]
mod tests {
    use super::SLACK_FORMATTING_HINT;

    #[test]
    fn hint_mentions_mrkdwn() {
        assert!(
            SLACK_FORMATTING_HINT.contains("mrkdwn"),
            "SLACK_FORMATTING_HINT must mention Slack mrkdwn"
        );
    }

    #[test]
    fn hint_points_to_slack_mrkdwn_not_generic_markdown() {
        assert!(
            SLACK_FORMATTING_HINT.contains("NOT Markdown"),
            "Slack hint should reject generic Markdown"
        );
        assert!(
            SLACK_FORMATTING_HINT.contains("NEVER **text**"),
            "Slack hint should reject double-asterisk bold"
        );
    }

    #[test]
    fn hint_documents_slack_mrkdwn_primitives() {
        for rule in [
            "Bold: *text*",
            "Italic: _text_",
            "Code inline: `text`",
            "Code block: triple backticks",
            "Strikethrough: ~text~",
            "Links: <url|label>",
            "<#CHANNEL_ID>",
            "<@USER_ID>",
        ] {
            assert!(
                SLACK_FORMATTING_HINT.contains(rule),
                "Slack hint missing mrkdwn rule: {rule}"
            );
        }
    }

    #[test]
    fn hint_documents_slack_unsupported_markdown_shapes() {
        for rule in [
            "Slack has no real list syntax",
            "NO ## headers",
            "NO horizontal rules",
            "NO tables",
            "NO HTML tags",
        ] {
            assert!(
                SLACK_FORMATTING_HINT.contains(rule),
                "Slack hint missing unsupported-shape rule: {rule}"
            );
        }
    }

    #[test]
    fn hint_wrapped_in_output_format() {
        assert!(
            SLACK_FORMATTING_HINT.starts_with("<output_format>"),
            "must open with <output_format> tag"
        );
        assert!(
            SLACK_FORMATTING_HINT.ends_with("</output_format>"),
            "must close with </output_format> tag"
        );
    }

    #[test]
    fn hint_contains_no_control_chars() {
        for c in SLACK_FORMATTING_HINT.chars() {
            assert!(
                !c.is_control() || c == '\n',
                "disallowed control char in SLACK_FORMATTING_HINT: U+{:04X}",
                c as u32
            );
        }
    }
}

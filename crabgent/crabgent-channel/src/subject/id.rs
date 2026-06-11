//! Reversible percent-encoding for the `<channel>:<participant>` subject id.
//!
//! Channel and participant ids may contain the `:` delimiter (or a literal
//! `%`), so both components are percent-encoded on build and decoded on
//! parse. Split out of the parent module to keep it under the LOC cap.

/// Build a stable channel subject id from adapter and participant ids.
///
/// The wire shape remains `<channel>:<participant>`. Components percent-encode
/// `:` and `%` so channel and participant ids remain round-trippable when they
/// contain the delimiter.
#[must_use]
pub fn channel_subject_id(channel: &str, participant_id: &str) -> String {
    format!(
        "{}:{}",
        encode_subject_component(channel),
        encode_subject_component(participant_id)
    )
}

/// Parse a subject id produced by [`channel_subject_id`].
///
/// Returns `None` for ids without a delimiter, invalid percent escapes, or
/// empty components.
#[must_use]
pub fn parse_channel_subject_id(id: &str) -> Option<(String, String)> {
    let (channel, participant_id) = id.split_once(':')?;
    let channel = decode_subject_component(channel)?;
    let participant_id = decode_subject_component(participant_id)?;
    if channel.trim().is_empty() || participant_id.trim().is_empty() {
        return None;
    }
    Some((channel, participant_id))
}

fn encode_subject_component(input: &str) -> String {
    let mut out = String::with_capacity(input.len());
    for ch in input.chars() {
        match ch {
            ':' => out.push_str("%3A"),
            '%' => out.push_str("%25"),
            other => out.push(other),
        }
    }
    out
}

fn decode_subject_component(input: &str) -> Option<String> {
    let mut out = String::with_capacity(input.len());
    let mut chars = input.chars();
    while let Some(ch) = chars.next() {
        if ch != '%' {
            out.push(ch);
            continue;
        }
        let hi = chars.next()?;
        let lo = chars.next()?;
        out.push(decode_subject_escape(hi, lo)?);
    }
    Some(out)
}

const fn decode_subject_escape(hi: char, lo: char) -> Option<char> {
    match (hi, lo) {
        ('2', '5') => Some('%'),
        ('3', 'A' | 'a') => Some(':'),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crabgent_core::subject::Subject;

    #[test]
    fn subject_with_colon_in_participant_id_round_trips_safely() {
        let id = channel_subject_id("slack", "user:with:colon");
        assert_eq!(id, "slack:user%3Awith%3Acolon");
        let subject = Subject::try_new(id).expect("encoded id is valid");
        let (channel, participant_id) =
            parse_channel_subject_id(subject.id()).expect("subject id parses");
        assert_eq!(channel, "slack");
        assert_eq!(participant_id, "user:with:colon");
    }

    #[test]
    fn subject_id_encoding_is_deterministic() {
        let first = channel_subject_id("slack:enterprise", "user%with:colon");
        let second = channel_subject_id("slack:enterprise", "user%with:colon");
        assert_eq!(first, second);
        assert_eq!(first, "slack%3Aenterprise:user%25with%3Acolon");
        assert_eq!(
            parse_channel_subject_id(&first),
            Some(("slack:enterprise".to_owned(), "user%with:colon".to_owned()))
        );
    }

    #[test]
    fn subject_id_parser_rejects_malformed_escapes() {
        assert!(parse_channel_subject_id("slack:user%XX").is_none());
        assert!(parse_channel_subject_id("slack").is_none());
        assert!(parse_channel_subject_id(":user").is_none());
    }
}

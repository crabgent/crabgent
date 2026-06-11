//! CLI parsing for the model command.

use crabgent_command::CommandError;
use crabgent_core::ModelId;

/// Parsed model command arguments.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CliArgs {
    List,
    Get { id: ModelId },
    Set { id: ModelId },
}

impl CliArgs {
    /// Parse a simple whitespace-separated model command.
    pub fn parse(input: &str) -> Result<Self, CommandError> {
        let mut parts = input.split_whitespace();
        let Some(op) = parts.next() else {
            return Err(CommandError::InvalidArgs(
                "model command requires a subcommand: list, get, or set".to_owned(),
            ));
        };
        match op {
            "list" => parse_no_more(parts, Self::List, "model list"),
            "get" => parse_model_id(parts, "model get").map(|id| Self::Get { id }),
            "set" => parse_model_id(parts, "model set").map(|id| Self::Set { id }),
            other => Err(CommandError::InvalidArgs(format!(
                "unknown model subcommand: {other}"
            ))),
        }
    }
}

fn parse_model_id<'a>(
    mut parts: impl Iterator<Item = &'a str>,
    context: &str,
) -> Result<ModelId, CommandError> {
    let Some(id) = parts.next() else {
        return Err(CommandError::InvalidArgs(format!(
            "{context}: missing model id"
        )));
    };
    if parts.next().is_some() {
        return Err(CommandError::InvalidArgs(format!(
            "{context}: unexpected extra arguments"
        )));
    }
    Ok(ModelId::new(id))
}

fn parse_no_more<'a>(
    mut parts: impl Iterator<Item = &'a str>,
    parsed: CliArgs,
    context: &str,
) -> Result<CliArgs, CommandError> {
    if parts.next().is_some() {
        return Err(CommandError::InvalidArgs(format!(
            "{context}: unexpected extra arguments"
        )));
    }
    Ok(parsed)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_list() {
        assert_eq!(CliArgs::parse("list").expect("parse list"), CliArgs::List);
    }

    #[test]
    fn parse_get_with_id() {
        assert_eq!(
            CliArgs::parse("get sonnet").expect("parse get"),
            CliArgs::Get {
                id: ModelId::new("sonnet"),
            },
        );
    }

    #[test]
    fn parse_set_with_id() {
        assert_eq!(
            CliArgs::parse("set gpt-5.5").expect("parse set"),
            CliArgs::Set {
                id: ModelId::new("gpt-5.5"),
            },
        );
    }

    #[test]
    fn parse_set_rejects_missing_id() {
        let err = CliArgs::parse("set").expect_err("missing id");

        // `safe_reply` is intentionally generic to avoid leaking user
        // input into the assistant reply (security.md). Assert on the
        // Display impl, which surfaces the operator-facing diagnostic.
        assert!(format!("{err}").contains("missing model id"));
    }

    #[test]
    fn parse_unknown_subcommand_errors() {
        let err = CliArgs::parse("remove sonnet").expect_err("unknown op");

        assert!(format!("{err}").contains("unknown model subcommand"));
    }

    #[test]
    fn safe_reply_for_invalid_args_is_generic_and_omits_user_input() {
        // "set /etc/secret extra-arg" reaches parse_model_id with
        // "/etc/secret" as id token and "extra-arg" as the trailing
        // token, hitting the InvalidArgs("unexpected extra arguments")
        // branch. Single-token inputs like "set secret-model-id" succeed
        // (any non-empty string is a valid ModelId), so the extra arg is
        // required to force the InvalidArgs path.
        let err = CliArgs::parse("set /etc/secret extra-arg").expect_err("must reject extra args");
        let reply = err.safe_reply();
        // safe_reply is the user-surface message; it must NOT echo user
        // input. Display surfaces operator diagnostics and is allowed to
        // contain detail; safe_reply must stay opaque per security.md.
        // The asserts below pin the security contract (non-empty, short,
        // no path or secret echo) without locking the exact wording, so
        // harmless rewording does not break the test.
        assert!(
            !reply.is_empty() && reply.len() < 50,
            "safe_reply must be short and non-empty, got {reply:?}"
        );
        assert!(
            !reply.contains('/'),
            "safe_reply must not echo path-like user input"
        );
        assert!(
            !reply.contains("secret"),
            "safe_reply must not echo user-supplied tokens"
        );
    }
}

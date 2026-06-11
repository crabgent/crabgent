# crabgent-channel-slack

Slack channel adapter for crabgent.

This crate provides:

- Slack Web API client helpers for messages, reactions, uploads, search, and Socket Mode URL creation.
- Socket Mode event ingestion with ACK deadlines, reconnect backoff, and listener isolation.
- `SlackChannel` for the generic `crabgent-channel::Channel` trait.
- `SlackInbox` glue for forwarding Slack events into a channel inbox while allowing custom event listeners.
- LLM tool implementations for send, read, react, edit, delete, upload, and search.

Configuration is injected through `SlackConfig`; production code does not read environment variables. Hosts provide one app token and one bot token per agent or kernel instance. Slack owners use the canonical `slack:T123/C456` encoding.

Thread replies map to `MessageRef::thread_root`. Slack broadcast replies map to `MessageRef::broadcast` and the Web API `reply_broadcast` field.

The integration test helpers live under `tests/common` and use local `wiremock` fixtures.

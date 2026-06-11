//! Interactive TUI client.
//!
//! `crabgent tui [agent] [--host h:p]` connects to a running daemon's
//! `/tui/<agent>` WebSocket and drives a chat session against the live agent
//! kernel. The daemon streams `Event` JSON frames; this client renders the
//! token / reasoning / tool-call stream like a coding-agent harness.
//!
//! This is a thin client: it owns no kernel. It attaches to the already
//! running agents, exactly like Element attaches to a Matrix room.
//!
//! Rendering follows the codex/claude-code model: an INLINE viewport (raw
//! mode, no alternate screen) keeps a small composer pinned at the bottom,
//! and every finalized line is pushed into the terminal's own scrollback via
//! `Terminal::insert_before`. The user keeps native scrollback; reasoning,
//! tool calls and the answer stream in line by line above the composer. The
//! pinned viewport is a codex-style input band plus a status line carrying
//! agent, host, state and cumulative token usage.

use std::collections::{HashSet, VecDeque};
use std::fmt::{self, Write as _};
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use crossterm::Command;
use crossterm::SynchronizedUpdate;
use crossterm::cursor::{Hide, MoveTo, Show};
use crossterm::event::{
    self, Event as TermEvent, KeyCode, KeyEvent, KeyEventKind, KeyModifiers,
    KeyboardEnhancementFlags, PopKeyboardEnhancementFlags, PushKeyboardEnhancementFlags,
};
use crossterm::execute;
use crossterm::terminal::{Clear, ClearType};
use futures::{SinkExt, StreamExt};
use ratatui::Terminal;
use ratatui::backend::Backend;
use ratatui::layout::{Constraint, Direction, Layout, Position, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Paragraph, Widget, Wrap};
use ratatui::{TerminalOptions, Viewport};
use serde::Deserialize;
use tokio_tungstenite::connect_async;
use tokio_tungstenite::tungstenite::Message as WsMessage;
use tokio_tungstenite::tungstenite::client::IntoClientRequest;

/// A streamed frame from the daemon: the serde-tagged `crabgent_core::Event`
/// plus the bridge's `usage` and `turn_error` extensions.
#[derive(Deserialize)]
#[serde(tag = "kind", content = "data", rename_all = "snake_case")]
enum WireEvent {
    Token(String),
    Reasoning(String),
    ToolCallStarted(ToolCallWire),
    ToolCallCompleted(ToolDoneWire),
    Notification(serde_json::Value),
    ServerToolResult(serde_json::Value),
    AttemptFailed(serde_json::Value),
    Final(String),
    TurnError(String),
    /// Per-turn token usage, sent by the bridge after the turn completes.
    Usage(UsageWire),
    /// Effective model + reasoning effort + override sources, sent on
    /// connect and after each turn.
    Status(StatusWire),
    /// Human-facing result of a slash command (`/compact`, `/model`, ...).
    Notice(String),
    /// Background task and cron progress from upstream observers.
    Activity(ActivityWire),
    /// Persisted TUI session replay, sent once when a WebSocket connects.
    History(HistoryWire),
}

/// Slash-command templates the composer completes and the help line lists.
/// A trailing space marks a command that expects an argument.
const COMMANDS: &[&str] = &[
    "/compact",
    "/model",
    "/model list",
    "/model set ",
    "/model clear",
    "/model effort ",
    "/goal ",
    "/goal pause",
    "/goal resume",
    "/goal clear",
    "/agent ",
    "/session ",
    "/session main",
    "/help",
];

/// Command templates whose text starts with the current input (and differ
/// from it), for the completion strip. Empty when `input` is not a command.
fn completions(input: &str) -> Vec<&'static str> {
    if !input.starts_with('/') || input.contains('\n') {
        return Vec::new();
    }
    COMMANDS
        .iter()
        .copied()
        .filter(|c| c.starts_with(input) && *c != input)
        .collect()
}

#[derive(Deserialize)]
struct UsageWire {
    #[serde(default)]
    input: u64,
    #[serde(default)]
    output: u64,
    #[serde(default)]
    cache_read: u64,
}

#[derive(Deserialize)]
struct StatusWire {
    #[serde(default)]
    model: String,
    /// `config-default` / `session-override` / `global-override`.
    #[serde(default)]
    model_source: String,
    #[serde(default)]
    provider: Option<String>,
    /// `low` / `medium` / `high`, or absent when the model has no knob.
    #[serde(default)]
    effort: Option<String>,
    /// `config` / `session-override` / `global-override` / `model-default`.
    #[serde(default)]
    effort_source: String,
    #[serde(default)]
    goal: Option<GoalStatusWire>,
}

#[derive(Deserialize)]
struct GoalStatusWire {
    #[serde(default)]
    objective: String,
    #[serde(default)]
    status: String,
    #[serde(default)]
    tokens_used: i64,
    #[serde(default)]
    token_budget: Option<i64>,
    #[serde(default)]
    time_used_seconds: i64,
}

#[derive(Debug, Clone, Deserialize)]
struct ActivityWire {
    #[serde(default)]
    agent: Option<String>,
    #[serde(default)]
    source: String,
    #[serde(default)]
    id: String,
    #[serde(default)]
    state: String,
    #[serde(default)]
    line: String,
}

#[derive(Debug, Clone, Deserialize)]
struct HistoryWire {
    #[serde(default)]
    session_id: String,
    #[serde(default)]
    items: Vec<HistoryItemWire>,
}

#[derive(Debug, Clone, Deserialize)]
struct HistoryItemWire {
    #[serde(default)]
    role: String,
    #[serde(default)]
    text: String,
}

#[derive(Deserialize)]
struct ToolCallWire {
    name: String,
    #[serde(default)]
    args: serde_json::Value,
}

#[derive(Deserialize)]
struct ToolDoneWire {
    call: ToolCallWire,
    result: ToolResultWire,
}

#[derive(Deserialize)]
struct ToolResultWire {
    #[serde(default)]
    output: serde_json::Value,
    #[serde(default)]
    is_error: bool,
}

/// Pinned inline viewport: 1 activity strip + growing input box + 1 status bar.
const MIN_INPUT_TEXT_ROWS: u16 = 1;
const MAX_INPUT_TEXT_ROWS: u16 = 8;
const INPUT_BOX_VERTICAL_PADDING: u16 = 2;
const MIN_OVERLAY_ROWS: u16 = 1;
const STATUS_BAR_ROWS: u16 = 1;
const MAX_QUEUED_INPUT_ROWS: usize = 3;
const MIN_VIEWPORT_HEIGHT: u16 =
    MIN_OVERLAY_ROWS + INPUT_BOX_VERTICAL_PADDING + MIN_INPUT_TEXT_ROWS + STATUS_BAR_ROWS;
const COMPOSER_MARGIN: u16 = 1;
const INPUT_MARKER_WIDTH: u16 = 2;
const FIRST_INPUT_MARKER: &str = "› ";
const CONTINUATION_INPUT_MARKER: &str = "  ";
const INPUT_BLOCK_BG: Color = Color::Rgb(64, 68, 75);
const STATUS_BLOCK_BG: Color = Color::Rgb(26, 28, 34);

/// Spinner frames for the working indicator.
const SPINNER: [&str; 10] = ["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"];

/// Streaming state for the in-flight turn. Finalized lines go straight to
/// scrollback, so the App only buffers the trailing not-yet-newline text.
/// Live WebSocket connection state, shown in the status line and driving the
/// reconnect loop.
#[derive(PartialEq, Eq)]
enum ConnState {
    /// Socket is up.
    Connected,
    /// Socket is down; the run loop is retrying with backoff.
    Reconnecting,
}

#[derive(Clone)]
struct GoalStatus {
    objective: String,
    status: String,
    tokens_used: i64,
    token_budget: Option<i64>,
    time_used_seconds: i64,
}

struct App {
    /// Agent name + daemon host, shown in the status line.
    agent: String,
    host: String,
    session: Option<String>,
    /// Live connection state.
    conn: ConnState,
    input: String,
    cursor: usize,
    busy: bool,
    busy_since: Option<Instant>,
    spinner: usize,
    queued_inputs: VecDeque<String>,
    /// Trailing partial of the streamed assistant text (before the next `\n`).
    asst_buf: String,
    /// Trailing partial of the streamed reasoning text.
    reason_buf: String,
    /// `true` once at least one assistant line has been committed this turn.
    asst_started: bool,
    reason_started: bool,
    /// Name of the tool currently running, surfaced in the status line.
    active_tool: Option<String>,
    /// Currently active background activity ids, maintained from observer
    /// start/progress/terminal frames and shown as compact status counts.
    active_tasks: HashSet<String>,
    active_crons: HashSet<String>,
    /// Session-cumulative token counts, shown in the status line. `tokens_in`
    /// excludes cache reads; `tokens_cache_read` keeps that API-reported
    /// subtotal visible without making fresh input look inflated.
    tokens_in: u64,
    tokens_cache_read: u64,
    tokens_out: u64,
    /// Effective model + reasoning effort + their override sources, from
    /// the bridge's `status` frame. `model` is `None` until the first
    /// frame arrives.
    model: Option<String>,
    model_source: String,
    effort: Option<String>,
    effort_source: String,
    goal: Option<GoalStatus>,
    /// Submitted-line ring for Up/Down recall.
    history: Vec<String>,
    /// Position while browsing `history` (None = editing the live input).
    hist_pos: Option<usize>,
    /// Live input stashed when history browsing starts, restored past the end.
    draft: String,
    draft_cursor: usize,
    /// Stable prefix being cycled by Tab, set on the first Tab and cleared
    /// on any edit so cycling does not reset itself after a replacement.
    comp_anchor: Option<String>,
    /// Tab cycle position over the anchor's completions.
    comp_idx: usize,
    /// Session id already replayed into scrollback for this client process.
    replayed_session_id: Option<String>,
}

impl App {
    #[cfg(test)]
    fn new(agent: String, host: String) -> Self {
        Self::with_session(agent, host, None)
    }

    fn with_session(agent: String, host: String, session: Option<String>) -> Self {
        Self {
            agent,
            host,
            session,
            conn: ConnState::Reconnecting,
            input: String::new(),
            cursor: 0,
            busy: false,
            busy_since: None,
            spinner: 0,
            queued_inputs: VecDeque::new(),
            asst_buf: String::new(),
            reason_buf: String::new(),
            asst_started: false,
            reason_started: false,
            active_tool: None,
            active_tasks: HashSet::new(),
            active_crons: HashSet::new(),
            tokens_in: 0,
            tokens_cache_read: 0,
            tokens_out: 0,
            model: None,
            model_source: String::new(),
            effort: None,
            effort_source: String::new(),
            goal: None,
            history: Vec::new(),
            hist_pos: None,
            draft: String::new(),
            draft_cursor: 0,
            comp_anchor: None,
            comp_idx: 0,
            replayed_session_id: None,
        }
    }

    fn session_label(&self) -> &str {
        self.session.as_deref().unwrap_or("main")
    }

    fn target_label(&self) -> String {
        self.session.as_ref().map_or_else(
            || self.agent.clone(),
            |session| format!("{}/{session}", self.agent),
        )
    }

    fn set_input(&mut self, input: String) {
        self.input = input;
        self.cursor = self.input.len();
    }

    fn clear_input(&mut self) {
        self.input.clear();
        self.cursor = 0;
        self.hist_pos = None;
        self.draft.clear();
        self.draft_cursor = 0;
        self.clear_completion();
    }

    fn clear_completion(&mut self) {
        self.comp_anchor = None;
        self.comp_idx = 0;
    }

    const fn record_usage(&mut self, usage: &UsageWire) {
        self.tokens_in = self
            .tokens_in
            .saturating_add(usage.input.saturating_sub(usage.cache_read));
        self.tokens_cache_read = self.tokens_cache_read.saturating_add(usage.cache_read);
        self.tokens_out = self.tokens_out.saturating_add(usage.output);
    }

    fn edit_started(&mut self) {
        self.clear_completion();
        self.hist_pos = None;
    }

    fn insert_char(&mut self, ch: char) {
        self.input.insert(self.cursor, ch);
        self.cursor += ch.len_utf8();
        self.edit_started();
    }

    fn insert_str(&mut self, text: &str) {
        self.input.insert_str(self.cursor, text);
        self.cursor += text.len();
        self.edit_started();
    }

    fn move_left(&mut self) {
        self.cursor = prev_char_boundary(&self.input, self.cursor);
    }

    fn move_right(&mut self) {
        self.cursor = next_char_boundary(&self.input, self.cursor);
    }

    fn move_word_left(&mut self) {
        self.cursor = previous_word_boundary(&self.input, self.cursor);
    }

    fn move_word_right(&mut self) {
        self.cursor = next_word_boundary(&self.input, self.cursor);
    }

    fn move_line_start(&mut self) {
        self.cursor = line_start(&self.input, self.cursor);
    }

    fn move_line_end(&mut self) {
        self.cursor = line_end(&self.input, self.cursor);
    }

    fn delete_prev_char(&mut self) {
        if self.cursor == 0 {
            return;
        }
        let start = prev_char_boundary(&self.input, self.cursor);
        self.input.drain(start..self.cursor);
        self.cursor = start;
        self.edit_started();
    }

    fn delete_next_char(&mut self) {
        if self.cursor >= self.input.len() {
            return;
        }
        let end = next_char_boundary(&self.input, self.cursor);
        self.input.drain(self.cursor..end);
        self.edit_started();
    }

    fn delete_prev_word(&mut self) {
        let start = previous_word_boundary(&self.input, self.cursor);
        if start == self.cursor {
            return;
        }
        self.input.drain(start..self.cursor);
        self.cursor = start;
        self.edit_started();
    }

    fn delete_to_line_start(&mut self) {
        let start = line_start(&self.input, self.cursor);
        if start == self.cursor {
            return;
        }
        self.input.drain(start..self.cursor);
        self.cursor = start;
        self.edit_started();
    }

    fn delete_to_line_end(&mut self) {
        let end = line_end(&self.input, self.cursor);
        if end == self.cursor {
            return;
        }
        self.input.drain(self.cursor..end);
        self.edit_started();
    }
}

fn clear_active_turn(app: &mut App) {
    app.asst_buf.clear();
    app.reason_buf.clear();
    app.asst_started = false;
    app.reason_started = false;
    app.active_tool = None;
    app.busy = false;
    app.busy_since = None;
    app.queued_inputs.clear();
}

fn begin_busy(app: &mut App, active_tool: Option<String>) {
    if !app.busy {
        app.busy_since = Some(Instant::now());
    }
    app.busy = true;
    app.active_tool = active_tool;
}

fn finish_active_turn(app: &mut App) {
    app.active_tool = None;
    if app.queued_inputs.is_empty() {
        app.busy = false;
        app.busy_since = None;
    } else {
        app.busy = true;
    }
}

fn can_queue_steering(app: &App, line: &str) -> bool {
    app.busy && !line.starts_with('/') && app.active_tool.as_deref() != Some("compact")
}

fn blocking_command_status(line: &str) -> Option<&'static str> {
    match line.trim() {
        "/compact" => Some("compact"),
        _ => None,
    }
}

fn activity_key(activity: &ActivityWire) -> String {
    format!("{}:{}", activity.source, activity.id)
}

fn update_activity_state(app: &mut App, activity: &ActivityWire) {
    let key = activity_key(activity);
    let set = match activity.source.as_str() {
        "task" => &mut app.active_tasks,
        "cron" => &mut app.active_crons,
        _ => return,
    };

    match activity.state.as_str() {
        "started" => {
            set.insert(key);
        }
        "progress" if activity.source == "task" => {
            set.insert(key);
        }
        "progress" if activity.source == "cron" && activity.id != "scheduler" => {
            set.insert(key);
        }
        "done" | "failed" | "cancelled" | "timed_out" => {
            set.remove(&key);
        }
        _ => {}
    }
}

fn activity_display_lines(activity: &ActivityWire, color: Color) -> Vec<Line<'static>> {
    const INLINE_MAX: usize = 140;
    const DETAIL_MAX: usize = 180;

    let text = if activity.line.trim().is_empty() {
        format!(
            "{} {} {}",
            activity.source,
            truncate(&activity.id, 8),
            activity.state
        )
    } else {
        activity.line.trim().to_owned()
    };
    let head_style = Style::default().fg(color);
    if let Some((head, detail)) = split_activity_preview(&text) {
        return vec![
            Line::styled(format!("• {}", truncate(head, INLINE_MAX)), head_style),
            Line::styled(
                format!("  └ {}", truncate(detail, DETAIL_MAX)),
                Style::default().fg(Color::Gray),
            ),
        ];
    }

    vec![Line::styled(
        format!("• {}", truncate(&text, INLINE_MAX)),
        head_style,
    )]
}

fn split_activity_preview(line: &str) -> Option<(&str, &str)> {
    const BYTE_PREVIEW_MARKER: &str = "B · ";
    let idx = line.find(BYTE_PREVIEW_MARKER)?;
    let head = line[..=idx].trim();
    let detail = line[idx + BYTE_PREVIEW_MARKER.len()..].trim();
    (!head.is_empty() && !detail.is_empty()).then_some((head, detail))
}

fn prev_char_boundary(text: &str, pos: usize) -> usize {
    if pos == 0 {
        return 0;
    }
    text[..pos].char_indices().last().map_or(0, |(idx, _)| idx)
}

fn next_char_boundary(text: &str, pos: usize) -> usize {
    if pos >= text.len() {
        return text.len();
    }
    pos + text[pos..].chars().next().map_or(0, char::len_utf8)
}

fn line_start(text: &str, pos: usize) -> usize {
    text[..pos].rfind('\n').map_or(0, |idx| idx + 1)
}

fn line_end(text: &str, pos: usize) -> usize {
    pos + text[pos..].find('\n').unwrap_or(text.len() - pos)
}

fn is_word_char(ch: char) -> bool {
    ch.is_alphanumeric() || ch == '_'
}

fn previous_word_boundary(text: &str, pos: usize) -> usize {
    let mut cur = pos;
    while cur > 0 {
        let prev = prev_char_boundary(text, cur);
        let ch = text[prev..cur].chars().next().unwrap_or(' ');
        if is_word_char(ch) {
            break;
        }
        cur = prev;
    }
    while cur > 0 {
        let prev = prev_char_boundary(text, cur);
        let ch = text[prev..cur].chars().next().unwrap_or(' ');
        if !is_word_char(ch) {
            break;
        }
        cur = prev;
    }
    cur
}

fn next_word_boundary(text: &str, pos: usize) -> usize {
    let mut cur = pos;
    while cur < text.len() {
        let next = next_char_boundary(text, cur);
        let ch = text[cur..next].chars().next().unwrap_or(' ');
        if is_word_char(ch) {
            break;
        }
        cur = next;
    }
    while cur < text.len() {
        let next = next_char_boundary(text, cur);
        let ch = text[cur..next].chars().next().unwrap_or(' ');
        if !is_word_char(ch) {
            break;
        }
        cur = next;
    }
    cur
}

#[cfg(test)]
fn cursor_line_col(text: &str, cursor: usize) -> (usize, usize) {
    let mut line = 0usize;
    let mut col = 0usize;
    for ch in text[..cursor].chars() {
        if ch == '\n' {
            line += 1;
            col = 0;
        } else {
            col += 1;
        }
    }
    (line, col)
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct InputRow {
    marker: &'static str,
    text: String,
    start: usize,
    end: usize,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct InputLayout {
    rows: Vec<InputRow>,
    cursor_row: usize,
    cursor_col: usize,
}

fn display_width(text: &str) -> usize {
    Span::raw(text).width()
}

fn input_content_width(prompt_width: u16) -> usize {
    prompt_width.saturating_sub(INPUT_MARKER_WIDTH).max(1) as usize
}

fn terminal_input_content_width(terminal_width: u16) -> usize {
    input_content_width(terminal_width.saturating_sub(COMPOSER_MARGIN))
}

const fn marker_for_line(line_idx: usize) -> &'static str {
    if line_idx == 0 {
        FIRST_INPUT_MARKER
    } else {
        CONTINUATION_INPUT_MARKER
    }
}

fn push_wrapped_input_rows(
    rows: &mut Vec<InputRow>,
    line_idx: usize,
    line_start: usize,
    line: &str,
    content_width: usize,
) {
    let first_marker = marker_for_line(line_idx);
    if line.is_empty() {
        rows.push(InputRow {
            marker: first_marker,
            text: String::new(),
            start: line_start,
            end: line_start,
        });
        return;
    }

    let mut row_start = 0usize;
    let mut row_width = 0usize;
    let mut marker = first_marker;
    for (idx, ch) in line.char_indices() {
        let next = idx + ch.len_utf8();
        let ch_width = display_width(&line[idx..next]);
        if row_width > 0 && row_width.saturating_add(ch_width) > content_width {
            rows.push(InputRow {
                marker,
                text: line[row_start..idx].to_owned(),
                start: line_start + row_start,
                end: line_start + idx,
            });
            row_start = idx;
            row_width = 0;
            marker = CONTINUATION_INPUT_MARKER;
        }
        row_width = row_width.saturating_add(ch_width);
    }

    rows.push(InputRow {
        marker,
        text: line[row_start..].to_owned(),
        start: line_start + row_start,
        end: line_start + line.len(),
    });
}

fn wrapped_input_layout(text: &str, cursor: usize, content_width: usize) -> InputLayout {
    let content_width = content_width.max(1);
    let mut rows = Vec::new();
    let mut line_start = 0usize;
    for (line_idx, line) in text.split('\n').enumerate() {
        push_wrapped_input_rows(&mut rows, line_idx, line_start, line, content_width);
        line_start = line_start.saturating_add(line.len()).saturating_add(1);
    }

    if let Some(last) = rows.last()
        && cursor == text.len()
        && last.end == cursor
        && last.start != last.end
        && display_width(&text[last.start..last.end]) >= content_width
    {
        rows.push(InputRow {
            marker: CONTINUATION_INPUT_MARKER,
            text: String::new(),
            start: cursor,
            end: cursor,
        });
    }

    let cursor_row = rows
        .iter()
        .enumerate()
        .find_map(|(idx, row)| {
            if cursor < row.end {
                return Some(idx);
            }
            if cursor == row.end && rows.get(idx + 1).is_none_or(|next| next.start != cursor) {
                return Some(idx);
            }
            None
        })
        .unwrap_or_else(|| rows.len().saturating_sub(1));
    let row = &rows[cursor_row];
    let cursor_col = if cursor >= row.start {
        display_width(&text[row.start..cursor.min(row.end)])
    } else {
        0
    };

    InputLayout {
        rows,
        cursor_row,
        cursor_col,
    }
}

fn visible_input_start_row(cursor_row: usize, rows: usize) -> usize {
    cursor_row.saturating_add(1).saturating_sub(rows.max(1))
}

fn input_text_rows_for_content_width(app: &App, content_width: usize) -> u16 {
    if app.input.is_empty() {
        return MIN_INPUT_TEXT_ROWS;
    }
    let layout = wrapped_input_layout(&app.input, app.cursor, content_width);
    u16::try_from(layout.rows.len())
        .unwrap_or(u16::MAX)
        .clamp(MIN_INPUT_TEXT_ROWS, MAX_INPUT_TEXT_ROWS)
}

const fn input_box_height_for_rows(text_rows: u16) -> u16 {
    text_rows + INPUT_BOX_VERTICAL_PADDING
}

#[cfg(test)]
const fn viewport_height(text_rows: u16) -> u16 {
    viewport_height_with_overlay(text_rows, MIN_OVERLAY_ROWS)
}

const fn viewport_height_with_overlay(text_rows: u16, overlay_rows: u16) -> u16 {
    overlay_rows + input_box_height_for_rows(text_rows) + STATUS_BAR_ROWS
}

fn overlay_rows(app: &App) -> u16 {
    if !app.busy {
        return MIN_OVERLAY_ROWS;
    }
    let visible_queue = app.queued_inputs.len().min(MAX_QUEUED_INPUT_ROWS);
    let overflow = usize::from(app.queued_inputs.len() > MAX_QUEUED_INPUT_ROWS);
    u16::try_from(3 + visible_queue + overflow).unwrap_or(u16::MAX)
}

fn desired_viewport_height(app: &App, terminal_width: u16) -> u16 {
    viewport_height_with_overlay(
        input_text_rows_for_content_width(app, terminal_input_content_width(terminal_width)),
        overlay_rows(app),
    )
    .max(MIN_VIEWPORT_HEIGHT)
}

fn format_elapsed(duration: Duration) -> String {
    let secs = duration.as_secs();
    if secs < 60 {
        return format!("{secs}s");
    }
    let mins = secs / 60;
    let rem = secs % 60;
    if mins < 60 {
        return format!("{mins}m {rem}s");
    }
    format!("{}h {}m", mins / 60, mins % 60)
}

fn working_line(app: &App) -> Line<'static> {
    let elapsed = app
        .busy_since
        .map_or_else(|| "0s".to_owned(), |since| format_elapsed(since.elapsed()));
    let label = app
        .active_tool
        .as_deref()
        .map_or_else(|| "Working".to_owned(), |tool| format!("Working: {tool}"));
    Line::from(vec![
        Span::styled("• ", Style::default().fg(Color::Yellow)),
        Span::styled(
            label,
            Style::default()
                .fg(Color::White)
                .add_modifier(Modifier::BOLD),
        ),
        Span::styled(
            format!(" ({elapsed} · esc to interrupt)"),
            Style::default().fg(Color::DarkGray),
        ),
    ])
}

fn queued_input_lines(app: &App) -> Vec<Line<'static>> {
    let mut lines = Vec::new();
    for input in app.queued_inputs.iter().take(MAX_QUEUED_INPUT_ROWS) {
        lines.push(Line::from(vec![
            Span::styled("  queued ", Style::default().fg(Color::DarkGray)),
            Span::styled("› ", Style::default().fg(Color::Cyan)),
            Span::styled(truncate(input, 120), Style::default().fg(Color::Gray)),
        ]));
    }
    let hidden = app
        .queued_inputs
        .len()
        .saturating_sub(MAX_QUEUED_INPUT_ROWS);
    if hidden > 0 {
        lines.push(Line::styled(
            format!("  queued +{hidden} more"),
            Style::default().fg(Color::DarkGray),
        ));
    }
    lines
}

fn steering_notice_count(text: &str, prefix: &str) -> Option<usize> {
    let rest = text.strip_prefix(prefix)?.trim();
    if rest.is_empty() {
        return Some(1);
    }
    rest.strip_prefix('(')
        .and_then(|value| value.strip_suffix(')'))
        .and_then(|value| value.parse().ok())
}

fn is_steering_queue_notice(text: &str) -> bool {
    text == "steering queued" || text.starts_with("steering queued for follow-up")
}

fn render_history<B: Backend>(terminal: &mut Terminal<B>, app: &mut App, history: HistoryWire) {
    if history.session_id.is_empty()
        || app.replayed_session_id.as_deref() == Some(history.session_id.as_str())
    {
        return;
    }
    app.replayed_session_id = Some(history.session_id);
    for item in history.items {
        let text = item.text.trim();
        if text.is_empty() {
            continue;
        }
        match item.role.as_str() {
            "user" => commit_user_input(terminal, text),
            "assistant" => commit_markdown_spaced(terminal, text),
            _ => commit_spaced(
                terminal,
                vec![Line::styled(
                    format!("• {text}"),
                    Style::default().fg(Color::DarkGray),
                )],
            ),
        }
    }
}

/// Push one finished block into the terminal scrollback above the viewport.
///
/// `Paragraph::line_count` is private in ratatui 0.29, so the wrapped height
/// is computed here: each logical line takes `ceil(display_width / width)`
/// rows (at least one). Char count is a safe proxy for display width; it
/// over-counts wide glyphs, which only ever reserves an extra blank row.
fn commit_with_style<B: Backend>(
    terminal: &mut Terminal<B>,
    lines: Vec<Line<'static>>,
    style: Style,
) {
    let width = terminal.size().map_or(80, |s| s.width).max(1) as usize;
    let mut height = 0usize;
    for line in &lines {
        let chars: usize = line.spans.iter().map(|s| s.content.chars().count()).sum();
        height += chars.div_ceil(width).max(1);
    }
    let height = u16::try_from(height).unwrap_or(u16::MAX).max(1);
    let para = Paragraph::new(lines)
        .style(style)
        .wrap(Wrap { trim: false });
    let _ = terminal.insert_before(height, |buf| para.render(buf.area, buf));
}

fn commit<B: Backend>(terminal: &mut Terminal<B>, lines: Vec<Line<'static>>) {
    commit_with_style(terminal, lines, Style::default());
}

fn commit_spaced<B: Backend>(terminal: &mut Terminal<B>, mut lines: Vec<Line<'static>>) {
    if lines.is_empty() {
        return;
    }
    lines.insert(0, Line::raw(""));
    commit(terminal, lines);
}

/// Render a finished assistant message as markdown into scrollback.
///
/// `tui_markdown::from_str` borrows the source string, so the parse +
/// render happen inside this fn while `md` is still owned; the resulting
/// `Text` never has to outlive the call.
fn commit_markdown<B: Backend>(terminal: &mut Terminal<B>, md: &str) {
    let width = terminal.size().map_or(80, |s| s.width).max(1) as usize;
    let text = tui_markdown::from_str(md);
    let mut height = 0usize;
    for line in &text.lines {
        height += line.width().div_ceil(width).max(1);
    }
    let height = u16::try_from(height.max(1)).unwrap_or(u16::MAX);
    let para = Paragraph::new(text).wrap(Wrap { trim: false });
    let _ = terminal.insert_before(height, |buf| para.render(buf.area, buf));
}

fn commit_markdown_spaced<B: Backend>(terminal: &mut Terminal<B>, md: &str) {
    commit(terminal, vec![Line::raw("")]);
    commit_markdown(terminal, md);
}

/// Flush every complete (`\n`-terminated) line out of `buf` into scrollback,
/// keeping the trailing partial. Continuation lines are indented under the
/// first line's marker.
fn flush_lines<B: Backend>(
    terminal: &mut Terminal<B>,
    buf: &mut String,
    started: &mut bool,
    style: Style,
    first: &str,
    cont: &str,
) {
    while let Some(idx) = buf.find('\n') {
        let line: String = buf[..idx].to_owned();
        buf.drain(..=idx);
        let marker = if *started { cont } else { first };
        *started = true;
        commit(
            terminal,
            vec![Line::styled(format!("{marker}{line}"), style)],
        );
    }
}

/// Flush the trailing partial (if any) and reset the per-turn marker state.
fn flush_final<B: Backend>(
    terminal: &mut Terminal<B>,
    buf: &mut String,
    started: &mut bool,
    style: Style,
    first: &str,
    cont: &str,
) {
    if !buf.is_empty() {
        let marker = if *started { cont } else { first };
        let text = std::mem::take(buf);
        commit(
            terminal,
            vec![Line::styled(format!("{marker}{text}"), style)],
        );
    }
    *started = false;
}

/// Fold one streamed wire event into scrollback + state. Returns `true` when
/// the turn finished (`Final` / `TurnError`).
#[allow(clippy::too_many_lines)] // one arm per Event variant; splitting hurts readability
fn apply<B: Backend>(terminal: &mut Terminal<B>, app: &mut App, ev: WireEvent) -> bool {
    let asst = Style::default().fg(Color::Green);
    let reason = Style::default()
        .fg(Color::DarkGray)
        .add_modifier(Modifier::ITALIC);
    match ev {
        WireEvent::Token(t) => {
            // Buffer the streamed answer; it is markdown-rendered as one
            // block on `Final` (codex-style), not trickled line by line.
            app.asst_buf.push_str(&t);
            false
        }
        WireEvent::Reasoning(t) => {
            app.reason_buf.push_str(&t);
            flush_lines(
                terminal,
                &mut app.reason_buf,
                &mut app.reason_started,
                reason,
                "· ",
                "  ",
            );
            false
        }
        WireEvent::ToolCallStarted(c) => {
            flush_final(
                terminal,
                &mut app.reason_buf,
                &mut app.reason_started,
                reason,
                "· ",
                "  ",
            );
            app.active_tool = Some(c.name.clone());
            commit_spaced(terminal, tool_started_lines(&c));
            false
        }
        WireEvent::ToolCallCompleted(d) => {
            app.active_tool = None;
            commit_spaced(terminal, tool_completed_lines(&d));
            false
        }
        WireEvent::Notification(v) => {
            if v.get("kind").and_then(serde_json::Value::as_str) == Some("tui_channel")
                && let Some(message) = v.get("message").and_then(serde_json::Value::as_str)
            {
                commit_markdown_spaced(terminal, message);
            } else {
                commit_spaced(
                    terminal,
                    vec![Line::styled(
                        format!("• {}", compact_json(&v)),
                        Style::default().fg(Color::Magenta),
                    )],
                );
            }
            false
        }
        WireEvent::ServerToolResult(v) => {
            commit_spaced(
                terminal,
                vec![Line::styled(
                    format!("• web search: {}", truncate(&compact_json(&v), 200)),
                    Style::default().fg(Color::Magenta),
                )],
            );
            false
        }
        WireEvent::AttemptFailed(v) => {
            commit_spaced(
                terminal,
                vec![Line::styled(
                    format!("• falling back: {}", compact_json(&v)),
                    Style::default().fg(Color::Magenta),
                )],
            );
            false
        }
        WireEvent::Usage(u) => {
            app.record_usage(&u);
            false
        }
        WireEvent::Status(s) => {
            app.model = (!s.model.is_empty()).then_some(s.model);
            app.model_source = s.model_source;
            app.effort = s.effort;
            app.effort_source = s.effort_source;
            app.goal = s.goal.map(|goal| GoalStatus {
                objective: goal.objective,
                status: goal.status,
                tokens_used: goal.tokens_used,
                token_budget: goal.token_budget,
                time_used_seconds: goal.time_used_seconds,
            });
            let _ = s.provider; // reserved for a future provider segment
            false
        }
        WireEvent::History(history) => {
            render_history(terminal, app, history);
            false
        }
        WireEvent::Notice(text) => {
            if let Some(count) = steering_notice_count(&text, "steering applied") {
                commit_queued_inputs(terminal, app, count);
                return false;
            }
            if is_steering_queue_notice(&text) {
                return false;
            }
            commit_spaced(
                terminal,
                vec![Line::styled(
                    format!("• {text}"),
                    Style::default().fg(Color::Cyan),
                )],
            );
            if app.active_tool.as_deref() == Some("compact") && text != "compacting session..." {
                finish_active_turn(app);
            }
            false
        }
        WireEvent::Activity(a) => {
            update_activity_state(app, &a);
            let _ = a.agent.as_deref();
            let color = match a.state.as_str() {
                "failed" | "timed_out" => Color::Red,
                "done" => Color::Green,
                "started" => Color::Cyan,
                _ => Color::DarkGray,
            };
            commit(terminal, activity_display_lines(&a, color));
            false
        }
        WireEvent::Final(text) => {
            flush_final(
                terminal,
                &mut app.reason_buf,
                &mut app.reason_started,
                reason,
                "· ",
                "  ",
            );
            // Prefer the streamed buffer; fall back to the final text for
            // non-streaming providers. Render the whole answer as markdown.
            let body = if app.asst_buf.is_empty() {
                text
            } else {
                std::mem::take(&mut app.asst_buf)
            };
            if !body.trim().is_empty() {
                commit_markdown_spaced(terminal, &body);
            }
            app.asst_started = false;
            finish_active_turn(app);
            true
        }
        WireEvent::TurnError(e) => {
            flush_final(
                terminal,
                &mut app.reason_buf,
                &mut app.reason_started,
                reason,
                "· ",
                "  ",
            );
            flush_final(
                terminal,
                &mut app.asst_buf,
                &mut app.asst_started,
                asst,
                "◆ ",
                "  ",
            );
            commit_spaced(
                terminal,
                vec![Line::styled(
                    format!("! {e}"),
                    Style::default().fg(Color::Red).add_modifier(Modifier::BOLD),
                )],
            );
            clear_active_turn(app);
            true
        }
    }
}

/// Compact a JSON value to a single-line preview for a tool card.
fn compact_json(v: &serde_json::Value) -> String {
    match v {
        serde_json::Value::String(s) => s.clone(),
        other => other.to_string(),
    }
}

fn tool_started_lines(call: &ToolCallWire) -> Vec<Line<'static>> {
    let mut lines = vec![Line::from(vec![
        Span::styled("•", Style::default().fg(Color::Yellow)),
        " ".into(),
        Span::styled(
            "Running",
            Style::default()
                .fg(Color::White)
                .add_modifier(Modifier::BOLD),
        ),
        " ".into(),
        Span::styled(call.name.clone(), Style::default().fg(Color::Yellow)),
    ])];

    let args = compact_json(&call.args);
    if !is_empty_tool_preview(&args) {
        lines.extend(prefixed_preview_lines(&args, 3, 180));
    }
    lines
}

fn tool_completed_lines(done: &ToolDoneWire) -> Vec<Line<'static>> {
    let (color, label) = if done.result.is_error {
        (Color::Red, "Failed")
    } else {
        (Color::Green, "Ran")
    };
    let mut lines = vec![Line::from(vec![
        Span::styled("•", Style::default().fg(color)),
        " ".into(),
        Span::styled(
            label,
            Style::default()
                .fg(Color::White)
                .add_modifier(Modifier::BOLD),
        ),
        " ".into(),
        Span::styled(done.call.name.clone(), Style::default().fg(color)),
    ])];

    let output = tool_output_text(&done.result.output);
    if output.trim().is_empty() {
        lines.push(Line::styled(
            "  └ (no output)",
            Style::default().fg(Color::DarkGray),
        ));
    } else {
        lines.extend(prefixed_preview_lines(&output, 5, 180));
    }
    lines
}

fn is_empty_tool_preview(text: &str) -> bool {
    matches!(text.trim(), "" | "{}" | "null")
}

fn tool_output_text(value: &serde_json::Value) -> String {
    match value {
        serde_json::Value::String(s) => s.clone(),
        serde_json::Value::Null => String::new(),
        other => other.to_string(),
    }
}

fn prefixed_preview_lines(text: &str, max_lines: usize, max_chars: usize) -> Vec<Line<'static>> {
    let lines = fold_preview_lines(text, max_lines, max_chars);
    if lines.is_empty() {
        return Vec::new();
    }

    let style = Style::default().fg(Color::DarkGray);
    lines
        .into_iter()
        .enumerate()
        .map(|(idx, line)| {
            let prefix = if idx == 0 { "  └ " } else { "    " };
            Line::styled(format!("{prefix}{line}"), style)
        })
        .collect()
}

fn fold_preview_lines(text: &str, max_lines: usize, max_chars: usize) -> Vec<String> {
    if max_lines == 0 {
        return Vec::new();
    }
    let raw: Vec<&str> = text.lines().collect();
    if raw.is_empty() {
        return Vec::new();
    }
    if raw.len() <= max_lines {
        return raw
            .into_iter()
            .map(|line| truncate_preserving_spaces(line, max_chars))
            .collect();
    }

    if max_lines == 1 {
        return vec![format!("… +{} lines", raw.len())];
    }

    let head_count = (max_lines - 1).div_ceil(2);
    let tail_count = max_lines - 1 - head_count;
    let omitted = raw.len().saturating_sub(head_count + tail_count);

    let mut out: Vec<String> = raw
        .iter()
        .take(head_count)
        .map(|line| truncate_preserving_spaces(line, max_chars))
        .collect();
    out.push(format!("… +{omitted} lines"));
    out.extend(
        raw.iter()
            .skip(raw.len() - tail_count)
            .map(|line| truncate_preserving_spaces(line, max_chars)),
    );
    out
}

fn truncate_preserving_spaces(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        return s.to_owned();
    }
    let cut: String = s.chars().take(max).collect();
    format!("{cut}…")
}

fn truncate(s: &str, max: usize) -> String {
    let one_line: String = s.split_whitespace().collect::<Vec<_>>().join(" ");
    if one_line.chars().count() <= max {
        one_line
    } else {
        let cut: String = one_line.chars().take(max).collect();
        format!("{cut}…")
    }
}

fn commit_user_input<B: Backend>(terminal: &mut Terminal<B>, input: &str) {
    let block_style = Style::default().bg(INPUT_BLOCK_BG);
    let mut lines = Vec::new();
    lines.push(Line::raw(""));
    lines.extend(input.split('\n').enumerate().map(|(idx, line)| {
        let marker = if idx == 0 { "› " } else { "  " };
        Line::from(vec![
            Span::styled(
                marker,
                block_style.fg(Color::Cyan).add_modifier(Modifier::BOLD),
            ),
            Span::styled(line.to_owned(), block_style.fg(Color::White)),
        ])
    }));
    lines.push(Line::raw(""));
    commit_with_style(terminal, lines, block_style);
}

fn commit_queued_inputs<B: Backend>(terminal: &mut Terminal<B>, app: &mut App, count: usize) {
    for _ in 0..count {
        let Some(input) = app.queued_inputs.pop_front() else {
            break;
        };
        commit_user_input(terminal, &input);
    }
}

const fn is_key_down(key: &KeyEvent) -> bool {
    matches!(key.kind, KeyEventKind::Press | KeyEventKind::Repeat)
}

const fn is_editor_newline_key(key: &KeyEvent) -> bool {
    matches!(key.code, KeyCode::Enter) && key.modifiers.contains(KeyModifiers::SHIFT)
        || matches!(key.code, KeyCode::Char('j')) && key.modifiers.contains(KeyModifiers::CONTROL)
        || matches!(key.code, KeyCode::Char('\n')) && key.modifiers.is_empty()
        || matches!(key.code, KeyCode::Enter) && key.modifiers.contains(KeyModifiers::ALT)
}

fn enable_keyboard_reporting() {
    let _ = execute!(
        std::io::stdout(),
        DisableModifyOtherKeys,
        PushKeyboardEnhancementFlags(
            KeyboardEnhancementFlags::DISAMBIGUATE_ESCAPE_CODES
                | KeyboardEnhancementFlags::REPORT_EVENT_TYPES
                | KeyboardEnhancementFlags::REPORT_ALTERNATE_KEYS,
        )
    );

    if tmux_should_enable_modify_other_keys() {
        let _ = execute!(std::io::stdout(), EnableModifyOtherKeys);
    }
}

fn restore_keyboard_reporting() {
    let _ = execute!(
        std::io::stdout(),
        PopKeyboardEnhancementFlags,
        ResetKeyboardEnhancementFlags,
        DisableModifyOtherKeys
    );
}

fn clear_terminal_for_startup() {
    use std::io::Write as _;

    let mut stdout = std::io::stdout();
    let _ = stdout.write_all(b"\x1b[r\x1b[0m\x1b[H\x1b[2J\x1b[3J\x1b[H");
    let _ = stdout.flush();
}

fn inline_terminal(height: u16) -> Result<ratatui::DefaultTerminal> {
    let backend = ratatui::backend::CrosstermBackend::new(std::io::stdout());
    Terminal::with_options(
        backend,
        TerminalOptions {
            viewport: Viewport::Inline(height),
        },
    )
    .context("resize tui inline viewport")
}

fn resize_inline_viewport(
    terminal: &mut ratatui::DefaultTerminal,
    current_height: &mut u16,
    next_height: u16,
) -> Result<()> {
    if *current_height == next_height {
        return Ok(());
    }
    let size = terminal
        .size()
        .context("read terminal size before resizing tui viewport")?;
    let old_area = terminal.get_frame().area();
    let old_top = old_area.y.min(size.height);
    let old_visible_height = old_area.height.min(size.height.saturating_sub(old_top));
    let old_bottom = old_top.saturating_add(old_visible_height).min(size.height);
    let next_visible_height = next_height.min(size.height);
    let mut viewport_top = old_top;

    if viewport_top.saturating_add(next_visible_height) > size.height {
        let scroll_by = viewport_top
            .saturating_add(next_visible_height)
            .saturating_sub(size.height);
        if scroll_by > 0 && old_top > 0 {
            execute!(
                std::io::stdout(),
                ScrollUpInRegion {
                    first_row: 0,
                    last_row: old_top.saturating_sub(1),
                    lines_to_scroll: scroll_by.min(old_top),
                }
            )
            .context("scroll output above growing tui viewport")?;
        }
        viewport_top = size.height.saturating_sub(next_visible_height);
    } else if next_height < *current_height {
        let new_bottom = viewport_top
            .saturating_add(next_visible_height)
            .min(size.height);
        let mut stdout = std::io::stdout();
        for y in new_bottom..old_bottom {
            let _ = execute!(stdout, MoveTo(0, y), Clear(ClearType::CurrentLine));
        }
    }

    execute!(std::io::stdout(), Hide).context("hide cursor before resizing tui viewport")?;
    terminal
        .set_cursor_position(Position {
            x: 0,
            y: viewport_top,
        })
        .context("position cursor before resizing tui viewport")?;
    *terminal = inline_terminal(next_height)?;
    *current_height = next_height;
    Ok(())
}

fn synchronized_terminal_update<T>(operation: impl FnOnce() -> Result<T>) -> Result<T> {
    let mut stdout = std::io::stdout();
    stdout
        .sync_update(|_| operation())
        .context("run synchronized tui update")?
}

fn tmux_should_enable_modify_other_keys() -> bool {
    tmux_should_enable_modify_other_keys_for(
        tmux_session_detected(
            std::env::var("TMUX").ok().as_deref(),
            std::env::var("TMUX_PANE").ok().as_deref(),
        ),
        read_tmux_extended_keys_format().as_deref(),
    )
}

const fn tmux_session_detected(tmux: Option<&str>, tmux_pane: Option<&str>) -> bool {
    tmux.is_some() || tmux_pane.is_some()
}

fn tmux_should_enable_modify_other_keys_for(
    running_in_tmux_session: bool,
    extended_keys_format: Option<&str>,
) -> bool {
    running_in_tmux_session && matches!(extended_keys_format, Some("csi-u"))
}

fn read_tmux_extended_keys_format() -> Option<String> {
    for args in [
        ["display-message", "-p", "#{extended-keys-format}"],
        ["show-options", "-gqv", "extended-keys-format"],
    ] {
        let output = std::process::Command::new("tmux")
            .args(args)
            .stdin(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .output()
            .ok()?;

        if !output.status.success() {
            continue;
        }

        if let Some(value) = String::from_utf8(output.stdout)
            .ok()
            .map(|value| value.trim().to_owned())
            .filter(|value| !value.is_empty())
        {
            return Some(value);
        }
    }

    None
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct ResetKeyboardEnhancementFlags;

impl Command for ResetKeyboardEnhancementFlags {
    fn write_ansi(&self, f: &mut impl fmt::Write) -> fmt::Result {
        f.write_str("\x1b[<u")
    }

    #[cfg(windows)]
    fn execute_winapi(&self) -> std::io::Result<()> {
        Err(std::io::Error::new(
            std::io::ErrorKind::Unsupported,
            "keyboard enhancement reset is not implemented for the legacy Windows API",
        ))
    }

    #[cfg(windows)]
    fn is_ansi_code_supported(&self) -> bool {
        false
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct EnableModifyOtherKeys;

impl Command for EnableModifyOtherKeys {
    fn write_ansi(&self, f: &mut impl fmt::Write) -> fmt::Result {
        f.write_str("\x1b[>4;2m")
    }

    #[cfg(windows)]
    fn execute_winapi(&self) -> std::io::Result<()> {
        Err(std::io::Error::new(
            std::io::ErrorKind::Unsupported,
            "modifyOtherKeys enable is not implemented for the legacy Windows API",
        ))
    }

    #[cfg(windows)]
    fn is_ansi_code_supported(&self) -> bool {
        false
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct DisableModifyOtherKeys;

impl Command for DisableModifyOtherKeys {
    fn write_ansi(&self, f: &mut impl fmt::Write) -> fmt::Result {
        f.write_str("\x1b[>4;0m")
    }

    #[cfg(windows)]
    fn execute_winapi(&self) -> std::io::Result<()> {
        Err(std::io::Error::new(
            std::io::ErrorKind::Unsupported,
            "modifyOtherKeys reset is not implemented for the legacy Windows API",
        ))
    }

    #[cfg(windows)]
    fn is_ansi_code_supported(&self) -> bool {
        false
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct ScrollUpInRegion {
    first_row: u16,
    last_row: u16,
    lines_to_scroll: u16,
}

impl Command for ScrollUpInRegion {
    fn write_ansi(&self, f: &mut impl fmt::Write) -> fmt::Result {
        if self.lines_to_scroll == 0 {
            return Ok(());
        }

        write!(
            f,
            "\x1b[{};{}r\x1b[{}S\x1b[r",
            self.first_row.saturating_add(1),
            self.last_row.saturating_add(1),
            self.lines_to_scroll
        )
    }

    #[cfg(windows)]
    fn execute_winapi(&self) -> std::io::Result<()> {
        Err(std::io::Error::new(
            std::io::ErrorKind::Unsupported,
            "scroll region is not implemented for the legacy Windows API",
        ))
    }

    #[cfg(windows)]
    fn is_ansi_code_supported(&self) -> bool {
        true
    }
}

/// Humanize a token count codex-style: `216K`, `4.87M`, `60.7M`.
#[allow(
    clippy::cast_precision_loss,
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss
)]
fn humanize(n: u64) -> String {
    if n < 1_000 {
        n.to_string()
    } else if n < 1_000_000 {
        let k = n as f64 / 1_000.0;
        if k < 10.0 {
            format!("{k:.2}K")
        } else if k < 100.0 {
            format!("{k:.1}K")
        } else {
            format!("{}K", k.round() as u64)
        }
    } else {
        let m = n as f64 / 1_000_000.0;
        if m < 10.0 {
            format!("{m:.2}M")
        } else {
            format!("{m:.1}M")
        }
    }
}

fn goal_status_segment(goal: &GoalStatus) -> String {
    let objective = truncate(&goal.objective, 28);
    let elapsed = format_elapsed(Duration::from_secs(
        u64::try_from(goal.time_used_seconds.max(0)).unwrap_or(0),
    ));
    let tokens = u64::try_from(goal.tokens_used.max(0)).unwrap_or(0);
    goal.token_budget.map_or_else(
        || {
            format!(
                "goal {} · {} · {} · {}",
                goal.status,
                objective,
                humanize(tokens),
                elapsed
            )
        },
        |budget| {
            let budget = u64::try_from(budget.max(0)).unwrap_or(0);
            format!(
                "goal {} · {} · {}/{} · {}",
                goal.status,
                objective,
                humanize(tokens),
                humanize(budget),
                elapsed
            )
        },
    )
}

const TUI_MAIN_SESSION: &str = "main";
const TUI_SESSION_MAX_CHARS: usize = 80;

pub fn normalize_session_arg(session: Option<&str>) -> Result<Option<String>> {
    session
        .map(normalize_session_name)
        .transpose()
        .map(Option::flatten)
}

fn normalize_session_name(raw: &str) -> Result<Option<String>> {
    let name = raw.trim();
    if name.is_empty() || name.eq_ignore_ascii_case(TUI_MAIN_SESSION) {
        return Ok(None);
    }
    if name.chars().count() > TUI_SESSION_MAX_CHARS {
        anyhow::bail!("session name too long; max {TUI_SESSION_MAX_CHARS} chars");
    }
    if name.chars().any(char::is_control) {
        anyhow::bail!("session name must not contain control characters");
    }
    if name.contains('/') {
        anyhow::bail!("session name must not contain '/'");
    }
    Ok(Some(name.to_owned()))
}

fn session_query_suffix(session: Option<&str>) -> String {
    session.map_or_else(String::new, |name| {
        format!("&session={}", query_component(name))
    })
}

fn query_component(value: &str) -> String {
    let mut out = String::new();
    for byte in value.bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(char::from(byte));
            }
            _ => {
                let _ = write!(out, "%{byte:02X}");
            }
        }
    }
    out
}

/// Outcome of one connected session's event loop.
enum LoopOutcome {
    /// User quit (Ctrl-C) or the daemon closed the socket.
    Quit,
    /// User asked to reconnect to a different agent on the same host via
    /// `/agent <name>`.
    Switch(String),
    /// User asked to switch named TUI sessions for the current agent.
    SwitchSession(Option<String>),
    /// The connection dropped; the run loop should reconnect to the same
    /// agent with backoff instead of exiting.
    Lost,
}

/// Entry point for the `tui` subcommand.
///
/// Owns the terminal and the input-reader thread for the whole session and
/// reconnects in place when the user switches agents with `/agent <name>`;
/// the new agent's bearer token is re-resolved from `config_path`. The host
/// is fixed for the session: `/agent` switches between agents served by the
/// same daemon. Use a fresh `tui <agent> --host h:p` to reach a different
/// daemon (e.g. the remote agents).
#[allow(clippy::too_many_lines)] // single tokio::select! event loop; cohesive as one fn
pub async fn run(
    config_path: std::path::PathBuf,
    agent: String,
    host: String,
    token: String,
    session: Option<String>,
) -> Result<()> {
    let mut cur_agent = agent;
    let mut cur_token = token;
    let mut cur_session = session;
    // The App (history, token counts, last-known status) persists across
    // reconnects to the same agent; a `/agent` switch makes a fresh one. It
    // starts in the reconnecting state; the first connect flips it to
    // Connected.
    let mut app = App::with_session(cur_agent.clone(), host.clone(), cur_session.clone());

    // Inline viewport: raw mode, NO alternate screen, so the user's native
    // scrollback is preserved and `insert_before` writes real history. The
    // terminal + the blocking input reader outlive individual connections so
    // an agent switch reconnects the WebSocket without re-grabbing the TTY.
    clear_terminal_for_startup();
    let initial_width = crossterm::terminal::size().map_or(80, |(width, _)| width);
    let mut viewport_height = desired_viewport_height(&app, initial_width);
    let mut terminal = ratatui::init_with_options(TerminalOptions {
        viewport: Viewport::Inline(viewport_height),
    });
    enable_keyboard_reporting();
    // crossterm's async `EventStream` returns `None` immediately on this
    // platform/tmux combo, so read terminal events on a blocking thread.
    let (input_tx, mut input_rx) = tokio::sync::mpsc::unbounded_channel::<TermEvent>();
    let stop = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    let reader_stop = std::sync::Arc::clone(&stop);
    let reader = std::thread::spawn(move || {
        while !reader_stop.load(std::sync::atomic::Ordering::Relaxed) {
            match event::poll(std::time::Duration::from_millis(100)) {
                Ok(true) => match event::read() {
                    Ok(ev) => {
                        if input_tx.send(ev).is_err() {
                            break;
                        }
                    }
                    Err(_) => break,
                },
                Ok(false) => {}
                Err(_) => break,
            }
        }
    });
    let mut spin = tokio::time::interval(std::time::Duration::from_millis(120));

    // Exponential backoff between reconnect attempts, capped and reset on a
    // successful connect. `announced` keeps the disconnect notice to one line
    // per outage instead of one per retry.
    let mut backoff = std::time::Duration::from_secs(1);
    let mut announced = false;
    let session_result: Result<()> = 'session: loop {
        let url = format!(
            "ws://{host}/tui/{cur_agent}?token={}{}",
            query_component(&cur_token),
            session_query_suffix(cur_session.as_deref())
        );
        let request = match url.into_client_request() {
            Ok(r) => r,
            Err(e) => {
                // A malformed ws URL cannot be fixed by retrying; surface it.
                break 'session Err(
                    anyhow::Error::from(e).context(format!("build ws request for {host}"))
                );
            }
        };
        let (mut ws_tx, mut ws_rx) = match connect_async(request).await {
            Ok((ws, _resp)) => {
                app.conn = ConnState::Connected;
                backoff = std::time::Duration::from_secs(1);
                announced = false;
                commit(
                    &mut terminal,
                    vec![Line::styled(
                        format!(
                            "• connected to {cur_agent} @ {host} · session {}",
                            app.session_label()
                        ),
                        Style::default().fg(Color::Cyan),
                    )],
                );
                ws.split()
            }
            Err(e) => {
                // Daemon unreachable: do NOT exit. Show the reconnecting state
                // and retry after the backoff, staying responsive to Esc.
                app.conn = ConnState::Reconnecting;
                clear_active_turn(&mut app);
                if !announced {
                    commit(
                        &mut terminal,
                        vec![Line::styled(
                            format!("! cannot reach {host} ({e}); auto-reconnecting"),
                            Style::default().fg(Color::Red),
                        )],
                    );
                    announced = true;
                }
                if idle_wait(&mut terminal, &mut app, &mut input_rx, &mut spin, backoff).await {
                    break 'session Ok(());
                }
                backoff = backoff
                    .saturating_mul(2)
                    .min(std::time::Duration::from_secs(15));
                continue 'session;
            }
        };

        let outcome: LoopOutcome = loop {
            if let Err(e) = synchronized_terminal_update(|| {
                let terminal_width = terminal.size().map_or(80, |s| s.width);
                resize_inline_viewport(
                    &mut terminal,
                    &mut viewport_height,
                    desired_viewport_height(&app, terminal_width),
                )?;
                terminal
                    .draw(|f| render(f, &app))
                    .context("draw tui frame")?;
                Ok(())
            }) {
                break 'session Err(e);
            }
            tokio::select! {
            maybe_ev = input_rx.recv() => {
                match maybe_ev {
                    Some(TermEvent::Key(key)) if is_key_down(&key) => {
                        match key.code {
                            KeyCode::Esc if app.busy => {
                                let frame = serde_json::json!({ "op": "cancel" }).to_string();
                                if ws_tx.send(WsMessage::Text(frame.into())).await.is_err() {
                                    commit(
                                        &mut terminal,
                                        vec![Line::styled(
                                            "! cancel failed; connection lost".to_owned(),
                                            Style::default().fg(Color::Red),
                                        )],
                                    );
                                    clear_active_turn(&mut app);
                                    break LoopOutcome::Lost;
                                }
                                clear_active_turn(&mut app);
                            }
                            KeyCode::Esc => {}
                            KeyCode::Char('c') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                                break LoopOutcome::Quit;
                            }
                            _ if is_editor_newline_key(&key) => {
                                app.insert_char('\n');
                            }
                            KeyCode::Enter => {
                                let line = app.input.trim().to_owned();
                                // `/agent <name>` is a client-side command:
                                // reconnect to another agent on this host
                                // instead of forwarding it to the bridge.
                                if !app.busy && (line == "/agent" || line.starts_with("/agent ")) {
                                    let name = line["/agent".len()..].trim().to_owned();
                                    commit_user_input(&mut terminal, &line);
                                    app.clear_input();
                                    if name.is_empty() {
                                        commit(
                                            &mut terminal,
                                            vec![Line::styled(
                                                "usage: /agent <name> — reconnect to another agent on this host".to_owned(),
                                                Style::default().fg(Color::DarkGray),
                                            )],
                                        );
                                    } else {
                                        break LoopOutcome::Switch(name);
                                    }
                                } else if line == "/session" || line.starts_with("/session ") {
                                    let name = line["/session".len()..].trim();
                                    commit_user_input(&mut terminal, &line);
                                    app.clear_input();
                                    if app.busy {
                                        commit(
                                            &mut terminal,
                                            vec![Line::styled(
                                                "cannot switch sessions while a turn is active".to_owned(),
                                                Style::default().fg(Color::DarkGray),
                                            )],
                                        );
                                    } else {
                                        match normalize_session_name(name) {
                                            Ok(next) if next == cur_session => {
                                                let label = next.as_deref().unwrap_or(TUI_MAIN_SESSION);
                                                commit(
                                                    &mut terminal,
                                                    vec![Line::styled(
                                                        format!("already in session {label}"),
                                                        Style::default().fg(Color::DarkGray),
                                                    )],
                                                );
                                            }
                                            Ok(next) => break LoopOutcome::SwitchSession(next),
                                            Err(e) => {
                                                commit(
                                                    &mut terminal,
                                                    vec![Line::styled(
                                                        format!("! cannot switch session: {e}"),
                                                        Style::default().fg(Color::Red),
                                                    )],
                                                );
                                            }
                                        }
                                    }
                                } else if !line.is_empty() {
                                    let queued = can_queue_steering(&app, &line);
                                    if queued {
                                        app.queued_inputs.push_back(line.clone());
                                    } else {
                                        commit_user_input(&mut terminal, &line);
                                    }
                                    if app.history.last() != Some(&line) {
                                        app.history.push(line.clone());
                                    }
                                    app.clear_input();
                                    if let Some(status) = blocking_command_status(&line) {
                                        begin_busy(&mut app, Some(status.to_owned()));
                                    } else if queued {
                                        let active_tool = app.active_tool.clone();
                                        begin_busy(&mut app, active_tool);
                                    } else if line.starts_with('/') {
                                        if !app.busy {
                                            app.active_tool = None;
                                            app.busy_since = None;
                                        }
                                    } else {
                                        begin_busy(&mut app, None);
                                    }
                                    let frame = if queued {
                                        serde_json::json!({
                                            "prompt": line,
                                            "steering": true,
                                        })
                                    } else {
                                        serde_json::json!({ "prompt": line })
                                    }
                                    .to_string();
                                    if ws_tx.send(WsMessage::Text(frame.into())).await.is_err() {
                                        commit(
                                            &mut terminal,
                                            vec![Line::styled(
                                                "! send failed; connection lost".to_owned(),
                                                Style::default().fg(Color::Red),
                                            )],
                                        );
                                        clear_active_turn(&mut app);
                                    }
                                }
                            }
                            // Tab cycles slash-command completions over a stable
                            // anchor (the prefix when the first Tab was pressed).
                            KeyCode::Tab => {
                                let anchor =
                                    app.comp_anchor.clone().unwrap_or_else(|| app.input.clone());
                                let comps = completions(&anchor);
                                if !comps.is_empty() {
                                    if app.comp_anchor.is_none() {
                                        app.comp_anchor = Some(anchor);
                                        app.comp_idx = 0;
                                    }
                                    let idx = app.comp_idx % comps.len();
                                    app.set_input(comps[idx].to_owned());
                                    app.comp_idx = app.comp_idx.wrapping_add(1);
                                }
                            }
                            // Up/Down walk the submitted-line history.
                            KeyCode::Up if !app.history.is_empty() => {
                                let pos = match app.hist_pos {
                                    None => {
                                        app.draft = app.input.clone();
                                        app.draft_cursor = app.cursor;
                                        app.history.len() - 1
                                    }
                                    Some(0) => 0,
                                    Some(p) => p - 1,
                                };
                                app.hist_pos = Some(pos);
                                app.set_input(app.history[pos].clone());
                                app.comp_anchor = None;
                            }
                            KeyCode::Down => {
                                if let Some(p) = app.hist_pos {
                                    if p + 1 < app.history.len() {
                                        app.hist_pos = Some(p + 1);
                                        app.set_input(app.history[p + 1].clone());
                                    } else {
                                        app.hist_pos = None;
                                        app.input = std::mem::take(&mut app.draft);
                                        app.cursor = app.draft_cursor.min(app.input.len());
                                    }
                                    app.comp_anchor = None;
                                }
                            }
                            KeyCode::Left
                                if key.modifiers.contains(KeyModifiers::ALT)
                                    || key.modifiers.contains(KeyModifiers::CONTROL) =>
                            {
                                app.move_word_left();
                            }
                            KeyCode::Right
                                if key.modifiers.contains(KeyModifiers::ALT)
                                    || key.modifiers.contains(KeyModifiers::CONTROL) =>
                            {
                                app.move_word_right();
                            }
                            KeyCode::Left => {
                                app.move_left();
                            }
                            KeyCode::Right => {
                                app.move_right();
                            }
                            KeyCode::Home => {
                                app.move_line_start();
                            }
                            KeyCode::End => {
                                app.move_line_end();
                            }
                            KeyCode::Char('a')
                                if key.modifiers.contains(KeyModifiers::CONTROL) =>
                            {
                                app.move_line_start();
                            }
                            KeyCode::Char('e')
                                if key.modifiers.contains(KeyModifiers::CONTROL) =>
                            {
                                app.move_line_end();
                            }
                            KeyCode::Char('b')
                                if key.modifiers.contains(KeyModifiers::ALT) =>
                            {
                                app.move_word_left();
                            }
                            KeyCode::Char('f')
                                if key.modifiers.contains(KeyModifiers::ALT) =>
                            {
                                app.move_word_right();
                            }
                            KeyCode::Char('w')
                                if key.modifiers.contains(KeyModifiers::CONTROL) =>
                            {
                                app.delete_prev_word();
                            }
                            KeyCode::Char('u')
                                if key.modifiers.contains(KeyModifiers::CONTROL) =>
                            {
                                app.delete_to_line_start();
                            }
                            KeyCode::Char('k')
                                if key.modifiers.contains(KeyModifiers::CONTROL) =>
                            {
                                app.delete_to_line_end();
                            }
                            KeyCode::Char('d')
                                if key.modifiers.contains(KeyModifiers::CONTROL) =>
                            {
                                app.delete_next_char();
                            }
                            KeyCode::Char(c)
                                if !key.modifiers.contains(KeyModifiers::CONTROL)
                                    && !key.modifiers.contains(KeyModifiers::ALT) =>
                            {
                                app.insert_char(c);
                            }
                            KeyCode::Backspace
                                if key.modifiers.contains(KeyModifiers::ALT)
                                    || key.modifiers.contains(KeyModifiers::CONTROL) =>
                            {
                                app.delete_prev_word();
                            }
                            KeyCode::Backspace => {
                                app.delete_prev_char();
                            }
                            KeyCode::Delete => {
                                app.delete_next_char();
                            }
                            _ => {}
                        }
                    }
                    Some(TermEvent::Paste(text)) => {
                        app.insert_str(&text);
                    }
                    Some(_) => {}
                    None => break LoopOutcome::Quit,
                }
            }
            maybe_frame = ws_rx.next() => {
                match maybe_frame {
                    Some(Ok(WsMessage::Text(t))) => {
                        match serde_json::from_str::<WireEvent>(&t) {
                            Ok(ev) => { apply(&mut terminal, &mut app, ev); }
                            Err(_) => commit(&mut terminal, vec![Line::raw(t.to_string())]),
                        }
                    }
                    Some(Ok(WsMessage::Close(_))) | None => {
                        // The connection dropped (a closed socket yields `None`
                        // on every later poll). Break to the reconnect loop
                        // instead of exiting; the outcome handler announces it
                        // once and retries with backoff.
                        clear_active_turn(&mut app);
                        break LoopOutcome::Lost;
                    }
                    Some(Ok(_)) => {}
                    Some(Err(_)) => {
                        // A transport error breaks the stream; reconnect.
                        clear_active_turn(&mut app);
                        break LoopOutcome::Lost;
                    }
                }
            }
            _ = spin.tick() => {
                if app.busy {
                    app.spinner = app.spinner.wrapping_add(1);
                }
            }
            }
        };

        match outcome {
            LoopOutcome::Quit => break 'session Ok(()),
            LoopOutcome::Switch(name) => match resolve_token(&config_path, &name) {
                Ok(t) => {
                    cur_agent = name;
                    cur_token = t;
                    // Fresh agent, fresh App (token counts + status reset);
                    // the next loop iteration connects to it.
                    app = App::with_session(cur_agent.clone(), host.clone(), cur_session.clone());
                    backoff = std::time::Duration::from_secs(1);
                    announced = false;
                }
                Err(e) => {
                    commit(
                        &mut terminal,
                        vec![Line::styled(
                            format!("! cannot switch to {name}: {e}"),
                            Style::default().fg(Color::Red),
                        )],
                    );
                }
            },
            LoopOutcome::SwitchSession(next) => {
                cur_session = next;
                app = App::with_session(cur_agent.clone(), host.clone(), cur_session.clone());
                backoff = std::time::Duration::from_secs(1);
                announced = false;
            }
            LoopOutcome::Lost => {
                app.conn = ConnState::Reconnecting;
                clear_active_turn(&mut app);
                if !announced {
                    commit(
                        &mut terminal,
                        vec![Line::styled(
                            "! connection lost; auto-reconnecting".to_owned(),
                            Style::default().fg(Color::Red),
                        )],
                    );
                    announced = true;
                }
                if idle_wait(&mut terminal, &mut app, &mut input_rx, &mut spin, backoff).await {
                    break 'session Ok(());
                }
                backoff = backoff
                    .saturating_mul(2)
                    .min(std::time::Duration::from_secs(15));
            }
        }
    };

    restore_keyboard_reporting();
    let _ = execute!(std::io::stdout(), Show);
    ratatui::restore();
    stop.store(true, std::sync::atomic::Ordering::Relaxed);
    let _ = reader.join();
    session_result
}

/// Idle for `dur` while keeping the viewport responsive between reconnect
/// attempts: it redraws (so the status line's reconnect spinner animates),
/// lets Ctrl-C quit, and ignores typing (there is no live socket to send to).
/// Returns `true` when the user asked to quit during the wait.
async fn idle_wait<B: Backend>(
    terminal: &mut Terminal<B>,
    app: &mut App,
    input_rx: &mut tokio::sync::mpsc::UnboundedReceiver<TermEvent>,
    spin: &mut tokio::time::Interval,
    dur: std::time::Duration,
) -> bool {
    let sleep = tokio::time::sleep(dur);
    tokio::pin!(sleep);
    loop {
        let _ = terminal.draw(|f| render(f, app));
        tokio::select! {
            maybe_ev = input_rx.recv() => match maybe_ev {
                Some(TermEvent::Key(key)) if is_key_down(&key) => match key.code {
                    KeyCode::Char('c') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                        return true;
                    }
                    _ => {}
                },
                Some(_) => {}
                None => return true,
            },
            () = &mut sleep => return false,
            _ = spin.tick() => app.spinner = app.spinner.wrapping_add(1),
        }
    }
}

/// Render the pinned inline viewport, codex-style: a spacer, a full-width
/// shaded input box (padded top and bottom), and a darker status bar. The
/// prompt and the status segments share one left margin so their left edges
/// line up.
#[allow(clippy::too_many_lines)] // one cohesive viewport paint; splitting hurts locality
fn render(f: &mut ratatui::Frame, app: &App) {
    use ratatui::widgets::Block;

    let min_input_box_height = input_box_height_for_rows(MIN_INPUT_TEXT_ROWS);
    let max_overlay_height = f
        .area()
        .height
        .saturating_sub(min_input_box_height + STATUS_BAR_ROWS)
        .max(MIN_OVERLAY_ROWS);
    let overlay_height = overlay_rows(app).min(max_overlay_height);
    let input_box_height = f
        .area()
        .height
        .saturating_sub(overlay_height + STATUS_BAR_ROWS)
        .max(min_input_box_height);
    let input_text_rows = input_box_height
        .saturating_sub(INPUT_BOX_VERTICAL_PADDING)
        .max(MIN_INPUT_TEXT_ROWS);
    let rows = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(overlay_height),   // working line / completions
            Constraint::Length(input_box_height), // shaded multiline input box
            Constraint::Length(STATUS_BAR_ROWS),  // status bar
        ])
        .split(f.area());

    // Overlay above the composer: while a turn is running it shows the
    // codex-style Working line and queued steering messages. When idle it
    // falls back to the slash-command completion strip.
    if app.busy {
        let mut overlay = vec![Line::raw(""), working_line(app)];
        overlay.extend(queued_input_lines(app));
        overlay.push(Line::raw(""));
        f.render_widget(Paragraph::new(overlay), rows[0]);
    } else if !completions(&app.input).is_empty() {
        let comps = completions(&app.input);
        let mut spans: Vec<Span> = Vec::new();
        for (i, c) in comps.iter().enumerate() {
            let style = if i == 0 {
                Style::default()
                    .fg(Color::Black)
                    .bg(Color::Cyan)
                    .add_modifier(Modifier::BOLD)
            } else {
                Style::default().fg(Color::DarkGray)
            };
            spans.push(Span::styled(format!(" {c} "), style));
            spans.push(Span::raw(" "));
        }
        let strip_row = Rect {
            x: rows[0].x + COMPOSER_MARGIN,
            y: rows[0].y,
            width: rows[0].width.saturating_sub(COMPOSER_MARGIN),
            height: 1,
        };
        f.render_widget(Paragraph::new(Line::from(spans)), strip_row);
    }

    // Input box: fill all rows with a medium shade; text scrolls to keep the
    // cursor line visible.
    let box_bg = Style::default().bg(INPUT_BLOCK_BG);
    f.render_widget(Block::default().style(box_bg), rows[1]);
    let prompt_area = Rect {
        x: rows[1].x + COMPOSER_MARGIN,
        y: rows[1].y + 1,
        width: rows[1].width.saturating_sub(COMPOSER_MARGIN),
        height: input_text_rows,
    };
    let composer = if app.input.is_empty() {
        vec![Line::from(vec![
            Span::styled(
                FIRST_INPUT_MARKER,
                box_bg.fg(Color::Cyan).add_modifier(Modifier::BOLD),
            ),
            Span::styled(
                format!("Nachricht an {}…", app.target_label()),
                box_bg.fg(Color::DarkGray),
            ),
        ])]
    } else {
        let layout = wrapped_input_layout(
            &app.input,
            app.cursor,
            input_content_width(prompt_area.width),
        );
        let visible_rows = usize::from(prompt_area.height.max(1));
        let start = visible_input_start_row(layout.cursor_row, visible_rows);
        let end = (start + visible_rows).min(layout.rows.len());
        layout.rows[start..end]
            .iter()
            .map(|row| {
                Line::from(vec![
                    Span::styled(
                        row.marker,
                        box_bg.fg(Color::Cyan).add_modifier(Modifier::BOLD),
                    ),
                    Span::styled(row.text.clone(), box_bg.fg(Color::White)),
                ])
            })
            .collect()
    };
    f.render_widget(Paragraph::new(composer).style(box_bg), prompt_area);

    // Status bar: darker full-width band, colour-segmented, same left margin.
    let bar_bg = Style::default().bg(STATUS_BLOCK_BG);
    f.render_widget(Block::default().style(bar_bg), rows[2]);
    let status_row = Rect {
        x: rows[2].x + COMPOSER_MARGIN,
        y: rows[2].y,
        width: rows[2].width.saturating_sub(COMPOSER_MARGIN),
        height: 1,
    };
    let sep = || Span::styled("  ·  ", bar_bg.fg(Color::DarkGray));
    let (state_text, state_color) = if app.conn == ConnState::Reconnecting {
        let s = SPINNER[app.spinner % SPINNER.len()];
        (format!("{s} reconnecting…"), Color::Red)
    } else if app.busy {
        let s = SPINNER[app.spinner % SPINNER.len()];
        app.active_tool.as_ref().map_or_else(
            || (format!("{s} working"), Color::Yellow),
            |tool| (format!("{s} {tool}"), Color::Yellow),
        )
    } else {
        ("Ready".to_owned(), Color::Green)
    };
    let mut spans = vec![
        Span::styled(
            app.agent.clone(),
            bar_bg.fg(Color::Magenta).add_modifier(Modifier::BOLD),
        ),
        sep(),
        Span::styled(
            format!("session {}", app.session_label()),
            bar_bg.fg(Color::Yellow),
        ),
        sep(),
        Span::styled(app.host.clone(), bar_bg.fg(Color::Cyan)),
        sep(),
        Span::styled(state_text, bar_bg.fg(state_color)),
    ];
    if !app.active_tasks.is_empty() {
        spans.push(sep());
        spans.push(Span::styled(
            format!("{} bg", app.active_tasks.len()),
            bar_bg.fg(Color::Yellow),
        ));
    }
    if !app.active_crons.is_empty() {
        spans.push(sep());
        spans.push(Span::styled(
            format!("{} cron", app.active_crons.len()),
            bar_bg.fg(Color::Yellow),
        ));
    }
    // Model + override source. An active session/global override is flagged
    // in yellow so it stands out from the plain config default.
    if let Some(model) = &app.model {
        spans.push(sep());
        spans.push(Span::styled(model.clone(), bar_bg.fg(Color::Cyan)));
        let tag = match app.model_source.as_str() {
            "session-override" => " session",
            "global-override" => " global",
            _ => "",
        };
        if !tag.is_empty() {
            spans.push(Span::styled(
                tag.to_owned(),
                bar_bg.fg(Color::Yellow).add_modifier(Modifier::BOLD),
            ));
        }
        // Reasoning effort + its source.
        spans.push(sep());
        let level = app.effort.as_deref().unwrap_or("off");
        spans.push(Span::styled(
            format!("effort {level}"),
            bar_bg.fg(Color::Blue),
        ));
        let (tag, tag_color) = match app.effort_source.as_str() {
            "session-override" => (" session", Color::Yellow),
            "global-override" => (" global", Color::Yellow),
            "config" => (" cfg", Color::Magenta),
            _ => ("", Color::DarkGray),
        };
        if !tag.is_empty() {
            spans.push(Span::styled(
                tag.to_owned(),
                bar_bg.fg(tag_color).add_modifier(Modifier::BOLD),
            ));
        }
    }
    if let Some(goal) = &app.goal {
        spans.push(sep());
        let color = match goal.status.as_str() {
            "active" => Color::Yellow,
            "complete" => Color::Green,
            "blocked" | "budget_limited" | "usage_limited" => Color::Red,
            "paused" => Color::DarkGray,
            _ => Color::Gray,
        };
        spans.push(Span::styled(goal_status_segment(goal), bar_bg.fg(color)));
    }
    spans.push(sep());
    spans.push(Span::styled(
        format!("{} in", humanize(app.tokens_in)),
        bar_bg.fg(Color::Blue),
    ));
    if app.tokens_cache_read > 0 {
        spans.push(sep());
        spans.push(Span::styled(
            format!("{} cached", humanize(app.tokens_cache_read)),
            bar_bg.fg(Color::DarkGray),
        ));
    }
    spans.push(sep());
    spans.push(Span::styled(
        format!("{} out", humanize(app.tokens_out)),
        bar_bg.fg(Color::Blue),
    ));
    let status = Line::from(spans);
    f.render_widget(Paragraph::new(status).style(bar_bg), status_row);

    let layout = wrapped_input_layout(
        &app.input,
        app.cursor,
        input_content_width(prompt_area.width),
    );
    let visible_rows = usize::from(prompt_area.height.max(1));
    let start = visible_input_start_row(layout.cursor_row, visible_rows);
    let cursor_y = prompt_area
        .y
        .saturating_add(u16::try_from(layout.cursor_row.saturating_sub(start)).unwrap_or(0));
    let cursor_x = prompt_area
        .x
        .saturating_add(INPUT_MARKER_WIDTH)
        .saturating_add(u16::try_from(layout.cursor_col).unwrap_or(0));
    f.set_cursor_position(Position::new(
        cursor_x.min(prompt_area.right().saturating_sub(1)),
        cursor_y.min(prompt_area.bottom().saturating_sub(1)),
    ));
}

/// Resolve the bearer token for `agent` from the config file. The TUI reads
/// the same config the daemon uses so the user does not pass secrets on the
/// command line.
pub fn resolve_token(config_path: &std::path::Path, agent: &str) -> Result<String> {
    let cfg = crate::config::Config::load_unresolved(config_path)?;
    let entry = cfg
        .agents
        .iter()
        .find(|a| a.name == agent)
        .with_context(|| format!("agent '{agent}' not found in {}", config_path.display()))?;
    let token = token_from_agent(entry).with_context(|| {
        format!("agent '{agent}' has no tui_bearer_token or mcp_bearer_token; TUI needs one")
    })?;
    crate::config::resolve_secret_if_ref(&token)
        .with_context(|| format!("resolve TUI bearer token for agent '{agent}'"))
}

/// Resolve the default TUI agent from the config. This keeps local installs
/// portable: the first configured agent is the default unless the CLI passes
/// an explicit agent name.
pub fn default_agent(config_path: &std::path::Path) -> Result<String> {
    let cfg = crate::config::Config::load_unresolved(config_path)?;
    cfg.agents
        .first()
        .map(|agent| agent.name.clone())
        .context("config has no [[agents]] entries")
}

fn token_from_agent(entry: &crate::config::Agent) -> Option<String> {
    entry
        .tui_bearer_token
        .clone()
        .or_else(|| entry.mcp_bearer_token.clone())
        .filter(|t| !t.trim().is_empty())
}

/// Default daemon host: the config's `mcp_server.bind`. Remote agents are
/// reached with an explicit `--host`.
#[must_use]
pub fn default_host(config_path: &std::path::Path) -> Option<String> {
    let cfg = crate::config::Config::load_unresolved(config_path).ok()?;
    cfg.mcp_server.map(|m| m.bind)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn truncate_collapses_and_caps() {
        assert_eq!(truncate("a  b\nc", 10), "a b c");
        assert_eq!(truncate("abcdef", 3), "abc…");
    }

    #[test]
    fn compact_json_unwraps_strings() {
        assert_eq!(compact_json(&serde_json::json!("hi")), "hi");
        assert_eq!(compact_json(&serde_json::json!({"a":1})), "{\"a\":1}");
    }

    #[test]
    fn tool_started_lines_use_codex_style_gutter() {
        let call = ToolCallWire {
            name: "bash".to_owned(),
            args: serde_json::json!({"cmd":"wc -l src/daemon.rs"}),
        };

        let lines = tool_started_lines(&call);

        assert_eq!(lines.len(), 2);
        assert_eq!(line_text(&lines[0]), "• Running bash");
        assert_eq!(
            line_text(&lines[1]),
            "  └ {\"cmd\":\"wc -l src/daemon.rs\"}"
        );
    }

    #[test]
    fn tool_completed_lines_fold_multiline_output() {
        let done = ToolDoneWire {
            call: ToolCallWire {
                name: "bash".to_owned(),
                args: serde_json::json!({}),
            },
            result: ToolResultWire {
                output: serde_json::json!("one\ntwo\nthree\nfour\nfive\nsix\nseven"),
                is_error: false,
            },
        };

        let lines = tool_completed_lines(&done);

        assert_eq!(line_text(&lines[0]), "• Ran bash");
        assert_eq!(line_text(&lines[1]), "  └ one");
        assert_eq!(line_text(&lines[2]), "    two");
        assert_eq!(line_text(&lines[3]), "    … +3 lines");
        assert_eq!(line_text(&lines[4]), "    six");
        assert_eq!(line_text(&lines[5]), "    seven");
    }

    fn line_text(line: &Line<'static>) -> String {
        line.spans
            .iter()
            .map(|span| span.content.as_ref())
            .collect()
    }

    #[test]
    fn humanize_scales() {
        assert_eq!(humanize(216), "216");
        assert_eq!(humanize(216_000), "216K");
        assert_eq!(humanize(4_870_000), "4.87M");
        assert_eq!(humanize(60_700_000), "60.7M");
    }

    #[test]
    fn format_elapsed_uses_compact_timer_units() {
        assert_eq!(format_elapsed(Duration::from_secs(9)), "9s");
        assert_eq!(format_elapsed(Duration::from_secs(92)), "1m 32s");
        assert_eq!(format_elapsed(Duration::from_secs(3_901)), "1h 5m");
    }

    #[test]
    fn steering_notice_count_parses_single_and_multiple() {
        assert_eq!(
            steering_notice_count("steering applied", "steering applied"),
            Some(1)
        );
        assert_eq!(
            steering_notice_count("steering applied (2)", "steering applied"),
            Some(2)
        );
        assert_eq!(
            steering_notice_count("steering queued for follow-up", "steering queued"),
            None
        );
    }

    #[test]
    fn wire_event_parses_token_and_final() {
        let t: WireEvent = serde_json::from_str(r#"{"kind":"token","data":"hi"}"#).unwrap();
        assert!(matches!(t, WireEvent::Token(s) if s == "hi"));
        let f: WireEvent = serde_json::from_str(r#"{"kind":"final","data":"done"}"#).unwrap();
        assert!(matches!(f, WireEvent::Final(s) if s == "done"));
    }

    #[test]
    fn wire_event_parses_usage() {
        let u: WireEvent = serde_json::from_str(
            r#"{"kind":"usage","data":{"input":100,"output":20,"cache_read":5}}"#,
        )
        .unwrap();
        assert!(matches!(u, WireEvent::Usage(w) if w.input == 100 && w.output == 20));
    }

    #[test]
    fn wire_event_parses_history() {
        let h: WireEvent = serde_json::from_str(
            r#"{"kind":"history","data":{"session_id":"session-1","items":[{"role":"user","text":"hi"}]}}"#,
        )
        .unwrap();

        assert!(matches!(
            h,
            WireEvent::History(w)
                if w.session_id == "session-1"
                    && w.items.len() == 1
                    && w.items[0].role == "user"
                    && w.items[0].text == "hi"
        ));
    }

    #[test]
    fn usage_accounting_splits_fresh_and_cached_input() {
        let mut app = App::new("local".to_owned(), "127.0.0.1:3100".to_owned());

        app.record_usage(&UsageWire {
            input: 100,
            output: 20,
            cache_read: 40,
        });
        app.record_usage(&UsageWire {
            input: 10,
            output: 2,
            cache_read: 20,
        });

        assert_eq!(app.tokens_in, 60);
        assert_eq!(app.tokens_cache_read, 60);
        assert_eq!(app.tokens_out, 22);
    }

    #[test]
    fn session_name_normalization_maps_empty_and_main_to_default() {
        assert_eq!(normalize_session_name("").unwrap(), None);
        assert_eq!(normalize_session_name(" main ").unwrap(), None);
        assert_eq!(
            normalize_session_name("moss rechnungen").unwrap(),
            Some("moss rechnungen".to_owned())
        );
    }

    #[test]
    fn session_name_normalization_rejects_path_separator() {
        assert!(normalize_session_name("foo/bar").is_err());
    }

    #[test]
    fn query_component_percent_encodes_session_names() {
        assert_eq!(query_component("moss rechnungen"), "moss%20rechnungen");
        assert_eq!(query_component("ä"), "%C3%A4");
    }

    #[test]
    fn wire_event_parses_activity() {
        let a: WireEvent = serde_json::from_str(
            r#"{"kind":"activity","data":{"source":"task","id":"abc","state":"started","line":"bg abc started"}}"#,
        )
        .unwrap();
        assert!(matches!(a, WireEvent::Activity(w) if w.id == "abc"));
    }

    #[test]
    fn activity_state_tracks_active_counts() {
        let mut app = App::new("local".to_owned(), "127.0.0.1:3100".to_owned());
        let started = ActivityWire {
            agent: Some("local".to_owned()),
            source: "task".to_owned(),
            id: "abc".to_owned(),
            state: "started".to_owned(),
            line: "bg abc started".to_owned(),
        };
        let done = ActivityWire {
            state: "done".to_owned(),
            ..started.clone()
        };

        update_activity_state(&mut app, &started);
        assert_eq!(app.active_tasks.len(), 1);

        update_activity_state(&mut app, &done);
        assert!(app.active_tasks.is_empty());
    }

    #[test]
    fn activity_display_splits_byte_preview() {
        let activity = ActivityWire {
            agent: Some("local".to_owned()),
            source: "task".to_owned(),
            id: "gemini-transcript-backlog-3w".to_owned(),
            state: "progress".to_owned(),
            line: "bg gemini-transcript-backlog-3w · final 515B · Erledigt: lokale mu-Suche und Wiki-Log-Abgleich".to_owned(),
        };

        let lines = activity_display_lines(&activity, Color::DarkGray);

        assert_eq!(lines.len(), 2);
        assert_eq!(
            lines[0].spans[0].content.as_ref(),
            "• bg gemini-transcript-backlog-3w · final 515B"
        );
        assert_eq!(
            lines[1].spans[0].content.as_ref(),
            "  └ Erledigt: lokale mu-Suche und Wiki-Log-Abgleich"
        );
    }

    #[test]
    fn activity_display_keeps_short_progress_inline() {
        let activity = ActivityWire {
            agent: Some("local".to_owned()),
            source: "task".to_owned(),
            id: "abc".to_owned(),
            state: "started".to_owned(),
            line: "bg demo task · started".to_owned(),
        };

        let lines = activity_display_lines(&activity, Color::Cyan);

        assert_eq!(lines.len(), 1);
        assert_eq!(
            lines[0].spans[0].content.as_ref(),
            "• bg demo task · started"
        );
    }

    #[test]
    fn token_from_agent_prefers_tui_token_and_falls_back_to_mcp() {
        let mut agent = crate::config::Agent {
            name: "crabgent".to_owned(),
            bot_token: None,
            bot_user_id: None,
            bot_username: None,
            pair_token: None,
            matrix: None,
            model: "gpt-5.5".to_owned(),
            system_prompt: "prompt".to_owned(),
            max_turns: None,
            holidays_country: None,
            holidays_subdivision: None,
            provider: crate::config::AgentProvider::OpenAi,
            fallback_models: Vec::new(),
            mcp_bearer_token: Some("mcp-token".to_owned()),
            tui_bearer_token: None,
            reasoning_effort: None,
            web_search: false,
            web_search_max_uses: None,
            tool_compact: false,
            tmux: crate::config::TmuxConfig::default(),
        };

        assert_eq!(token_from_agent(&agent).as_deref(), Some("mcp-token"));
        agent.tui_bearer_token = Some("tui-token".to_owned());
        assert_eq!(token_from_agent(&agent).as_deref(), Some("tui-token"));
    }

    #[test]
    fn clear_active_turn_resets_streaming_state() {
        let mut app = App::new("local".to_owned(), "127.0.0.1:3100".to_owned());
        app.busy = true;
        app.busy_since = Some(Instant::now());
        app.queued_inputs.push_back("later".to_owned());
        app.active_tool = Some("bash".to_owned());
        app.asst_buf = "partial answer".to_owned();
        app.reason_buf = "partial reasoning".to_owned();
        app.asst_started = true;
        app.reason_started = true;

        clear_active_turn(&mut app);

        assert!(!app.busy);
        assert!(app.busy_since.is_none());
        assert!(app.queued_inputs.is_empty());
        assert!(app.active_tool.is_none());
        assert!(app.asst_buf.is_empty());
        assert!(app.reason_buf.is_empty());
        assert!(!app.asst_started);
        assert!(!app.reason_started);
    }

    #[test]
    fn compact_is_a_blocking_slash_command() {
        assert_eq!(blocking_command_status("/compact"), Some("compact"));
        assert_eq!(blocking_command_status("  /compact  "), Some("compact"));
        assert_eq!(blocking_command_status("/model"), None);
        assert_eq!(blocking_command_status("/help"), None);
    }

    #[test]
    fn wire_event_parses_tool_calls() {
        let s: WireEvent = serde_json::from_str(
            r#"{"kind":"tool_call_started","data":{"name":"memory","args":{"op":"recall"}}}"#,
        )
        .unwrap();
        assert!(matches!(s, WireEvent::ToolCallStarted(c) if c.name == "memory"));
    }

    #[test]
    fn editor_inserts_and_deletes_at_cursor() {
        let mut app = App::new("local".to_owned(), "127.0.0.1:3100".to_owned());
        app.set_input("ac".to_owned());
        app.move_left();
        app.insert_char('b');
        assert_eq!(app.input, "abc");
        assert_eq!(app.cursor, 2);

        app.delete_prev_char();
        assert_eq!(app.input, "ac");
        assert_eq!(app.cursor, 1);

        app.delete_next_char();
        assert_eq!(app.input, "a");
        assert_eq!(app.cursor, 1);
    }

    #[test]
    fn editor_deletes_previous_word() {
        let mut app = App::new("local".to_owned(), "127.0.0.1:3100".to_owned());
        app.set_input("eins zwei drei".to_owned());

        app.delete_prev_word();
        assert_eq!(app.input, "eins zwei ");
        assert_eq!(app.cursor, "eins zwei ".len());

        app.delete_prev_word();
        assert_eq!(app.input, "eins ");
        assert_eq!(app.cursor, "eins ".len());
    }

    #[test]
    fn editor_jumps_words_and_handles_multiline() {
        let mut app = App::new("local".to_owned(), "127.0.0.1:3100".to_owned());
        app.set_input("eins\ndrei".to_owned());
        app.move_word_left();
        app.insert_str("zwei ");

        assert_eq!(app.input, "eins\nzwei drei");
        assert_eq!(cursor_line_col(&app.input, app.cursor), (1, 5));

        app.move_line_start();
        assert_eq!(app.cursor, "eins\n".len());
        app.move_line_end();
        assert_eq!(app.cursor, app.input.len());
    }

    #[test]
    fn viewport_height_grows_with_multiline_input() {
        let mut app = App::new("local".to_owned(), "127.0.0.1:3100".to_owned());
        assert_eq!(desired_viewport_height(&app, 80), MIN_VIEWPORT_HEIGHT);

        app.set_input("eins\nzwei\ndrei".to_owned());
        assert_eq!(input_text_rows_for_content_width(&app, 78), 3);
        assert_eq!(desired_viewport_height(&app, 80), viewport_height(3));

        app.set_input(
            (0..20)
                .map(|i| format!("line {i}"))
                .collect::<Vec<_>>()
                .join("\n"),
        );
        assert_eq!(
            input_text_rows_for_content_width(&app, 78),
            MAX_INPUT_TEXT_ROWS
        );
        assert_eq!(
            desired_viewport_height(&app, 80),
            viewport_height(MAX_INPUT_TEXT_ROWS)
        );
    }

    #[test]
    fn viewport_height_grows_with_soft_wrapped_input() {
        let mut app = App::new("local".to_owned(), "127.0.0.1:3100".to_owned());
        app.set_input("abcdefghi".to_owned());

        assert_eq!(input_text_rows_for_content_width(&app, 4), 3);
        assert_eq!(desired_viewport_height(&app, 7), viewport_height(3));
    }

    #[test]
    fn wrapped_input_layout_maps_cursor_to_soft_wraps() {
        let layout = wrapped_input_layout("abcde", "abcde".len(), 4);

        assert_eq!(layout.rows.len(), 2);
        assert_eq!(layout.rows[0].text, "abcd");
        assert_eq!(layout.rows[1].text, "e");
        assert_eq!(layout.cursor_row, 1);
        assert_eq!(layout.cursor_col, 1);
    }

    #[test]
    fn wrapped_input_layout_adds_cursor_row_at_exact_boundary() {
        let layout = wrapped_input_layout("abcd", "abcd".len(), 4);

        assert_eq!(layout.rows.len(), 2);
        assert_eq!(layout.rows[0].text, "abcd");
        assert_eq!(layout.rows[1].text, "");
        assert_eq!(layout.cursor_row, 1);
        assert_eq!(layout.cursor_col, 0);
    }

    #[test]
    fn viewport_height_keeps_growing_while_turn_is_busy() {
        let mut app = App::new("local".to_owned(), "127.0.0.1:3100".to_owned());
        app.busy = true;
        app.set_input("eins\nzwei\ndrei".to_owned());

        assert_eq!(input_text_rows_for_content_width(&app, 78), 3);
        assert_eq!(
            desired_viewport_height(&app, 80),
            viewport_height_with_overlay(3, 3)
        );
    }

    #[test]
    fn viewport_height_grows_with_queued_steering() {
        let mut app = App::new("local".to_owned(), "127.0.0.1:3100".to_owned());
        begin_busy(&mut app, None);
        app.queued_inputs.push_back("eins".to_owned());
        app.queued_inputs.push_back("zwei".to_owned());

        assert_eq!(overlay_rows(&app), 5);
        assert_eq!(
            desired_viewport_height(&app, 80),
            viewport_height_with_overlay(MIN_INPUT_TEXT_ROWS, 5)
        );
    }

    #[test]
    fn editor_newline_key_accepts_shift_enter_and_ctrl_j_fallback() {
        assert!(is_editor_newline_key(&KeyEvent::new(
            KeyCode::Enter,
            KeyModifiers::SHIFT
        )));
        assert!(is_editor_newline_key(&KeyEvent::new(
            KeyCode::Char('j'),
            KeyModifiers::CONTROL
        )));
        assert!(is_editor_newline_key(&KeyEvent::new(
            KeyCode::Char('\n'),
            KeyModifiers::NONE
        )));
        assert!(is_editor_newline_key(&KeyEvent::new(
            KeyCode::Enter,
            KeyModifiers::ALT
        )));
        assert!(!is_editor_newline_key(&KeyEvent::new(
            KeyCode::Enter,
            KeyModifiers::NONE
        )));
    }

    #[test]
    fn tmux_modify_other_keys_requires_tmux_and_csi_u() {
        assert!(tmux_session_detected(
            Some("/tmp/tmux-501/default,1,0"),
            None
        ));
        assert!(tmux_session_detected(None, Some("%0")));
        assert!(!tmux_session_detected(None, None));

        assert!(tmux_should_enable_modify_other_keys_for(
            true,
            Some("csi-u")
        ));
        assert!(!tmux_should_enable_modify_other_keys_for(
            true,
            Some("xterm")
        ));
        assert!(!tmux_should_enable_modify_other_keys_for(true, None));
        assert!(!tmux_should_enable_modify_other_keys_for(
            false,
            Some("csi-u")
        ));
    }

    #[test]
    fn keyboard_reporting_commands_match_xterm_sequences() {
        fn ansi_for(command: impl Command) -> String {
            let mut out = String::new();
            command.write_ansi(&mut out).unwrap();
            out
        }

        assert_eq!(ansi_for(EnableModifyOtherKeys), "\x1b[>4;2m");
        assert_eq!(ansi_for(DisableModifyOtherKeys), "\x1b[>4;0m");
        assert_eq!(ansi_for(ResetKeyboardEnhancementFlags), "\x1b[<u");
        assert_eq!(
            ansi_for(ScrollUpInRegion {
                first_row: 0,
                last_row: 19,
                lines_to_scroll: 3,
            }),
            "\x1b[1;20r\x1b[3S\x1b[r"
        );
    }
}

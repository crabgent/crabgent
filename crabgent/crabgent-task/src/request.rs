//! [`TaskRequest`]: input shape for [`crate::TaskExecutor::spawn`].

use crabgent_core::message::Message;
use crabgent_core::model::{ModelId, ModelTarget, ReasoningEffort};
use crabgent_core::{InvalidSubjectError, Subject, ToolAccess};
use crabgent_store::{Owner, SessionId, TaskId};

/// Input to [`crate::TaskExecutor::spawn`].
///
/// `owner` is used by the [`crabgent_store::TaskStore`] for filtering.
/// `subject` is forwarded to the kernel for policy decisions. They often
/// match (e.g. `Subject::try_new(owner.as_str())`) but the split lets policy
/// and persistence diverge when needed.
///
/// `messages` is the pre-assembled context the caller wants the kernel
/// to start with (e.g. recent thread, summary, or empty for a fresh
/// run). [`crate::TaskExecutor`] does not consult any
/// [`crabgent_store::SessionStore`] on its own; higher-level wrappers can
/// resolve `context_mode` into a `messages` vec before calling `spawn`.
#[derive(Debug, Clone)]
pub struct TaskRequest {
    pub owner: Owner,
    pub subject: Subject,
    pub name: Option<String>,
    pub prompt: String,
    /// Configured default model for the spawned run.
    pub model: ModelTarget,
    /// Explicit task-level model selector. When present, this wins over
    /// session and global model overrides.
    pub explicit_model: Option<ModelTarget>,
    /// Session-scoped override loaded by the caller that has active session
    /// context.
    pub session_model_override: Option<ModelId>,
    /// Explicit task-level reasoning effort snapshot. When present, this wins
    /// over session and global effort overrides.
    pub reasoning_effort: Option<ReasoningEffort>,
    pub system_prompt: Option<String>,
    pub messages: Vec<Message>,
    pub parent_session_id: Option<SessionId>,
    pub parent_task_id: Option<TaskId>,
    pub context_mode: Option<String>,
    pub max_turns: Option<u32>,
    pub tool_access: ToolAccess,
}

impl TaskRequest {
    /// Minimal constructor with an explicit task model.
    ///
    /// Panics if the derived subject is empty. Use [`Self::try_new`] for
    /// fallible input paths.
    pub fn new(owner: Owner, model: impl Into<ModelTarget>, prompt: impl Into<String>) -> Self {
        let model = model.into();
        Self::from_parts(owner, model.clone(), Some(model), prompt)
    }

    /// Fallible constructor for user-supplied owners.
    ///
    /// The subject is derived from `owner` and rejects empty or whitespace-only
    /// identities.
    pub fn try_new(
        owner: Owner,
        model: impl Into<ModelTarget>,
        prompt: impl Into<String>,
    ) -> Result<Self, InvalidSubjectError> {
        let model = model.into();
        Self::try_from_parts(owner, model.clone(), Some(model), prompt)
    }

    /// Constructor for callers that only know the default model. The spawned
    /// run may still be changed by session or global overrides.
    pub fn new_default(
        owner: Owner,
        model: impl Into<ModelTarget>,
        prompt: impl Into<String>,
    ) -> Self {
        Self::from_parts(owner, model.into(), None, prompt)
    }

    /// Fallible default-model constructor for user-supplied owners.
    pub fn try_new_default(
        owner: Owner,
        model: impl Into<ModelTarget>,
        prompt: impl Into<String>,
    ) -> Result<Self, InvalidSubjectError> {
        Self::try_from_parts(owner, model.into(), None, prompt)
    }

    fn try_from_parts(
        owner: Owner,
        model: ModelTarget,
        explicit_model: Option<ModelTarget>,
        prompt: impl Into<String>,
    ) -> Result<Self, InvalidSubjectError> {
        let subject = Subject::try_new(owner.as_str())?;
        Ok(Self {
            owner,
            subject,
            name: None,
            prompt: prompt.into(),
            model,
            explicit_model,
            session_model_override: None,
            reasoning_effort: None,
            system_prompt: None,
            messages: Vec::new(),
            parent_session_id: None,
            parent_task_id: None,
            context_mode: None,
            max_turns: None,
            tool_access: ToolAccess::default(),
        })
    }

    fn from_parts(
        owner: Owner,
        model: ModelTarget,
        explicit_model: Option<ModelTarget>,
        prompt: impl Into<String>,
    ) -> Self {
        let subject = Subject::new(owner.as_str());
        Self {
            owner,
            name: None,
            subject,
            prompt: prompt.into(),
            model,
            explicit_model,
            session_model_override: None,
            reasoning_effort: None,
            system_prompt: None,
            messages: Vec::new(),
            parent_session_id: None,
            parent_task_id: None,
            context_mode: None,
            max_turns: None,
            tool_access: ToolAccess::default(),
        }
    }

    /// Override the kernel subject (default is `Subject::try_new(owner.as_str())`).
    #[must_use]
    pub fn with_subject(mut self, subject: Subject) -> Self {
        self.subject = subject;
        self
    }

    /// Set a short display name for status UIs and task lists.
    #[must_use]
    pub fn with_name(mut self, name: impl Into<String>) -> Self {
        self.name = Some(name.into());
        self
    }

    /// Provide pre-assembled context messages.
    #[must_use]
    pub fn with_messages(mut self, messages: Vec<Message>) -> Self {
        self.messages = messages;
        self
    }

    /// Set an optional system prompt.
    #[must_use]
    pub fn with_system_prompt(mut self, prompt: impl Into<String>) -> Self {
        self.system_prompt = Some(prompt.into());
        self
    }

    /// Override the kernel's `max_turns`.
    #[must_use]
    pub const fn with_max_turns(mut self, max_turns: u32) -> Self {
        self.max_turns = Some(max_turns);
        self
    }

    /// Tag the task with a parent session.
    #[must_use]
    pub const fn with_parent_session(mut self, id: SessionId) -> Self {
        self.parent_session_id = Some(id);
        self
    }

    /// Carry the active session model override into the spawned run.
    #[must_use]
    pub fn with_session_model_override(mut self, model: impl Into<ModelId>) -> Self {
        self.session_model_override = Some(model.into());
        self
    }

    /// Carry the resolved task reasoning effort into the spawned run.
    #[must_use]
    pub const fn with_reasoning_effort(mut self, effort: ReasoningEffort) -> Self {
        self.reasoning_effort = Some(effort);
        self
    }

    /// Tag the task with a parent task.
    #[must_use]
    pub const fn with_parent_task(mut self, id: TaskId) -> Self {
        self.parent_task_id = Some(id);
        self
    }

    /// Attach a context-mode hint (opaque to the executor).
    #[must_use]
    pub fn with_context_mode(mut self, mode: impl Into<String>) -> Self {
        self.context_mode = Some(mode.into());
        self
    }

    /// Limit the tools advertised to and executable by the spawned run.
    #[must_use]
    pub fn with_tool_access(mut self, access: ToolAccess) -> Self {
        self.tool_access = access;
        self
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn new_derives_subject_from_owner() {
        let req = TaskRequest::new(Owner::new("alice"), "claude-haiku-4-5", "hi");
        assert_eq!(req.subject.id(), "alice");
        assert_eq!(req.owner, Owner::new("alice"));
        assert_eq!(req.model.as_str(), "claude-haiku-4-5");
        assert_eq!(req.explicit_model.as_ref(), Some(&req.model));
        assert!(req.session_model_override.is_none());
        assert!(req.reasoning_effort.is_none());
        assert_eq!(req.prompt, "hi");
        assert!(req.messages.is_empty());
        assert!(req.system_prompt.is_none());
        assert!(req.max_turns.is_none());
    }

    #[test]
    fn try_new_rejects_empty_owner_subject() {
        let err = TaskRequest::try_new(Owner::new(""), "m", "p").expect_err("empty rejected");
        assert_eq!(err, InvalidSubjectError);
    }

    #[test]
    fn new_default_sets_default_without_explicit_model() {
        let req = TaskRequest::new_default(Owner::new("alice"), "m", "p");

        assert_eq!(req.model.as_str(), "m");
        assert!(req.explicit_model.is_none());
    }

    #[test]
    fn with_session_model_override_sets_model_id() {
        let req = TaskRequest::new_default(Owner::new("alice"), "m", "p")
            .with_session_model_override("session-model");

        assert_eq!(
            req.session_model_override.as_ref().map(ModelId::as_str),
            Some("session-model")
        );
    }

    #[test]
    fn with_reasoning_effort_sets_snapshot() {
        let req = TaskRequest::new_default(Owner::new("alice"), "m", "p")
            .with_reasoning_effort(ReasoningEffort::High);

        assert_eq!(req.reasoning_effort, Some(ReasoningEffort::High));
    }

    #[test]
    #[should_panic(expected = "Subject id must not be empty")]
    fn new_panics_when_owner_subject_is_empty() {
        let _ = TaskRequest::new(Owner::new(""), "m", "p");
    }

    #[test]
    fn with_subject_overrides_default() {
        let req =
            TaskRequest::new(Owner::new("alice"), "m", "p").with_subject(Subject::new("svc-bot"));
        assert_eq!(req.subject.id(), "svc-bot");
        assert_eq!(req.owner, Owner::new("alice"));
    }

    #[test]
    fn with_messages_replaces_vec() {
        use crabgent_core::ContentBlock;
        let msgs = vec![Message::User {
            content: vec![ContentBlock::Text { text: "x".into() }],
            timestamp: None,
        }];
        let req = TaskRequest::new(Owner::new("u"), "m", "p").with_messages(msgs);
        assert_eq!(req.messages.len(), 1);
    }

    #[test]
    fn with_system_prompt_sets_some() {
        let req = TaskRequest::new(Owner::new("u"), "m", "p").with_system_prompt("be terse");
        assert_eq!(req.system_prompt.as_deref(), Some("be terse"));
    }

    #[test]
    fn with_max_turns_sets_value() {
        let req = TaskRequest::new(Owner::new("u"), "m", "p").with_max_turns(5);
        assert_eq!(req.max_turns, Some(5));
    }

    #[test]
    fn with_parent_session_and_task_set_ids() {
        let s = SessionId::new();
        let t = TaskId::new();
        let req = TaskRequest::new(Owner::new("u"), "m", "p")
            .with_parent_session(s.clone())
            .with_parent_task(t.clone());
        assert_eq!(req.parent_session_id.as_ref(), Some(&s));
        assert_eq!(req.parent_task_id.as_ref(), Some(&t));
    }

    #[test]
    fn with_context_mode_sets_hint() {
        let req = TaskRequest::new(Owner::new("u"), "m", "p").with_context_mode("recent_thread");
        assert_eq!(req.context_mode.as_deref(), Some("recent_thread"));
    }
}

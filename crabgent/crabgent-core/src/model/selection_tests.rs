use super::{
    AttemptKind, ModelInfo, ModelTarget, ReasoningEffort, ResolvedModel, request_for_attempt,
    resolve_attempts,
};
use crate::error::KernelError;
use crate::model::ModelRegistry;
use crate::types::{LlmRequest, WebSearchConfig};

fn registry_with(models: &[(&str, &str)]) -> ModelRegistry {
    let mut registry = ModelRegistry::new();
    for (provider, id) in models {
        registry
            .insert(ModelInfo::minimal(*id, *provider))
            .expect("unique test model inserts");
    }
    registry
}

fn web_search_config() -> WebSearchConfig {
    WebSearchConfig {
        enabled: true,
        max_uses: Some(2),
        allowed_domains: vec!["example.org".into()],
        blocked_domains: Vec::new(),
    }
}

fn base_request(web_search: WebSearchConfig) -> LlmRequest {
    LlmRequest {
        model: "base".into(),
        system_prompt: None,
        messages: Vec::new(),
        tools: Vec::new(),
        max_tokens: None,
        temperature: None,
        stop_sequences: Vec::new(),
        reasoning_effort: None,
        web_search,
        tool_choice: None,
    }
}

fn resolved_attempt(supports_web_search: bool) -> ResolvedModel {
    let mut info = ModelInfo::minimal("canonical", "provider");
    info.caps.supports_web_search = supports_web_search;
    ResolvedModel {
        target: ModelTarget::new("provider", "alias"),
        info,
    }
}

fn resolved_attempt_with_effort(effort: Option<ReasoningEffort>) -> ResolvedModel {
    let mut attempt = resolved_attempt(true);
    attempt.info.caps.reasoning_effort = effort;
    attempt
}

fn resolved_attempt_without_temperature() -> ResolvedModel {
    let mut info = ModelInfo::minimal("canonical", "provider");
    info.caps.supports_temperature = false;
    ResolvedModel {
        target: ModelTarget::new("provider", "alias"),
        info,
    }
}

fn base_request_with_effort(effort: Option<ReasoningEffort>) -> LlmRequest {
    LlmRequest {
        reasoning_effort: effort,
        ..base_request(WebSearchConfig::default())
    }
}

#[test]
fn request_for_attempt_applies_target_model_and_defaults() {
    let attempt = resolved_attempt(true);
    let info = attempt.info.clone();
    let req = request_for_attempt(
        &base_request(WebSearchConfig::default()),
        &attempt,
        AttemptKind::Primary,
    );

    assert_eq!(req.model.as_str(), "alias");
    assert_eq!(req.max_tokens, Some(info.caps.default_max_output_tokens));
    assert_eq!(req.temperature, Some(info.caps.default_temperature()));
}

#[test]
fn request_for_attempt_clears_web_search_when_attempt_caps_lack_support() {
    let req = request_for_attempt(
        &base_request(web_search_config()),
        &resolved_attempt(false),
        AttemptKind::Fallback,
    );

    assert_eq!(req.web_search, WebSearchConfig::default());
}

#[test]
fn request_for_attempt_keeps_web_search_when_attempt_caps_support() {
    let web_search = web_search_config();
    let req = request_for_attempt(
        &base_request(web_search.clone()),
        &resolved_attempt(true),
        AttemptKind::Fallback,
    );

    assert_eq!(req.web_search, web_search);
}

#[test]
fn request_for_attempt_keeps_web_search_on_primary_attempt_without_support() {
    let web_search = web_search_config();
    let req = request_for_attempt(
        &base_request(web_search.clone()),
        &resolved_attempt(false),
        AttemptKind::Primary,
    );

    assert_eq!(req.web_search, web_search);
}

#[test]
fn request_for_attempt_clears_reasoning_effort_when_attempt_caps_lack_support() {
    let req = request_for_attempt(
        &base_request_with_effort(Some(ReasoningEffort::Medium)),
        &resolved_attempt_with_effort(None),
        AttemptKind::Fallback,
    );

    assert_eq!(req.reasoning_effort, None);
}

#[test]
fn request_for_attempt_clears_disabled_reasoning_effort_on_unsupported_primary() {
    let req = request_for_attempt(
        &base_request_with_effort(Some(ReasoningEffort::Disabled)),
        &resolved_attempt_with_effort(None),
        AttemptKind::Primary,
    );

    assert_eq!(req.reasoning_effort, None);
}

#[test]
fn request_for_attempt_keeps_reasoning_effort_when_attempt_supports_it() {
    let req = request_for_attempt(
        &base_request_with_effort(Some(ReasoningEffort::Low)),
        &resolved_attempt_with_effort(Some(ReasoningEffort::High)),
        AttemptKind::Fallback,
    );

    assert_eq!(req.reasoning_effort, Some(ReasoningEffort::Low));
}

#[test]
fn request_for_attempt_defaults_reasoning_effort_from_caps_when_base_is_none() {
    let req = request_for_attempt(
        &base_request_with_effort(None),
        &resolved_attempt_with_effort(Some(ReasoningEffort::Medium)),
        AttemptKind::Fallback,
    );

    assert_eq!(req.reasoning_effort, Some(ReasoningEffort::Medium));
}

#[test]
fn request_for_attempt_strips_temperature_when_model_unsupported() {
    let req = request_for_attempt(
        &base_request(WebSearchConfig::default()),
        &resolved_attempt_without_temperature(),
        AttemptKind::Primary,
    );

    assert!(
        req.temperature.is_none(),
        "models with supports_temperature == false must not receive a temperature value",
    );
}

#[test]
fn request_for_attempt_keeps_temperature_when_model_supports_it() {
    let attempt = resolved_attempt(true);
    let info = attempt.info.clone();
    let req = request_for_attempt(
        &base_request(WebSearchConfig::default()),
        &attempt,
        AttemptKind::Primary,
    );

    assert_eq!(req.temperature, Some(info.caps.default_temperature()));
}

#[test]
fn resolve_attempts_drops_unknown_fallback_keeps_primary() {
    let registry = registry_with(&[("primary-provider", "primary")]);
    let attempts = resolve_attempts(
        &registry,
        &ModelTarget::id("primary"),
        &[ModelTarget::new("ghost", "deactivated")],
    )
    .expect("known primary must resolve even when a fallback is unknown");

    assert_eq!(attempts.len(), 1, "the unknown fallback must be dropped");
    assert_eq!(attempts[0].info.id.as_str(), "primary");
}

#[test]
fn resolve_attempts_keeps_only_resolvable_fallbacks() {
    let registry = registry_with(&[("primary-provider", "primary"), ("fb-provider", "fb")]);
    let attempts = resolve_attempts(
        &registry,
        &ModelTarget::id("primary"),
        &[
            ModelTarget::new("ghost", "gone"),
            ModelTarget::new("fb-provider", "fb"),
        ],
    )
    .expect("known primary must resolve");

    assert_eq!(attempts.len(), 2, "only the resolvable fallback survives");
    assert_eq!(attempts[0].info.id.as_str(), "primary");
    assert_eq!(attempts[1].info.id.as_str(), "fb");
}

#[test]
fn resolve_attempts_unknown_primary_is_fatal() {
    let registry = registry_with(&[("primary-provider", "primary")]);
    let err = resolve_attempts(&registry, &ModelTarget::id("missing"), &[])
        .expect_err("an unknown primary model must fail closed");

    assert!(matches!(err, KernelError::UnknownModel(_)), "got {err:?}");
}

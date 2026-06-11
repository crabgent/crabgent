use super::{
    GlobalModelOverrideStore, ModelId, ModelInfo, ModelOverrideStoreError, ModelRegistry,
    ModelTarget, ResolveModelError, ResolvedSource, effective_params, map_resolve_error,
    resolve_model, resolve_model_target, resolve_model_with_overrides,
};
use crate::error::KernelError;
use async_trait::async_trait;

#[derive(Default)]
struct TestGlobalStore {
    model: Option<ModelId>,
    fail: bool,
}

#[async_trait]
impl GlobalModelOverrideStore for TestGlobalStore {
    async fn get_global_model_override(&self) -> Result<Option<ModelId>, ModelOverrideStoreError> {
        if self.fail {
            return Err(ModelOverrideStoreError::backend("boom"));
        }
        Ok(self.model.clone())
    }

    async fn set_global_model_override(
        &self,
        _model: &ModelId,
    ) -> Result<(), ModelOverrideStoreError> {
        Err(ModelOverrideStoreError::backend(
            "resolution tests do not set global overrides",
        ))
    }

    async fn clear_global_model_override(&self) -> Result<(), ModelOverrideStoreError> {
        Err(ModelOverrideStoreError::backend(
            "resolution tests do not clear global overrides",
        ))
    }
}

fn registry_with(ids: &[&str]) -> ModelRegistry {
    let mut r = ModelRegistry::new();
    for id in ids {
        r.insert(ModelInfo::minimal(*id, "test"))
            .expect("test result");
    }
    r
}

#[test]
fn effective_params_fills_defaults_from_caps() {
    let info = ModelInfo::minimal("m", "test");
    let p = effective_params(None, None, &info);
    assert_eq!(p.max_tokens, info.caps.default_max_output_tokens);
    assert!((p.temperature - info.caps.default_temperature()).abs() < 1e-6);
}

#[test]
fn effective_params_respects_caller_overrides() {
    let info = ModelInfo::minimal("m", "test");
    let p = effective_params(Some(123), Some(0.25), &info);
    assert_eq!(p.max_tokens, 123);
    assert!((p.temperature - 0.25).abs() < 1e-6);
}

#[test]
fn resolve_model_returns_info_for_canonical() {
    let mut r = ModelRegistry::new();
    r.insert(ModelInfo::minimal("m", "test"))
        .expect("test result");
    let model = resolve_model(&r, &ModelTarget::id("m")).expect("test result");
    assert_eq!(model.target.provider(), Some("test"));
    assert_eq!(model.target.as_str(), "m");
    assert_eq!(model.info.id.as_str(), "m");
}

#[test]
fn resolve_model_maps_unknown_to_kernel_error() {
    let r = ModelRegistry::new();
    let err = resolve_model(&r, &ModelTarget::id("nope")).expect_err("expected error");
    match err {
        KernelError::UnknownModel(id) => assert_eq!(id.as_str(), "nope"),
        other => panic!("unexpected: {other:?}"),
    }
}

#[test]
fn resolve_model_maps_ambiguous_to_kernel_error() {
    let mut r = ModelRegistry::new();
    r.insert(ModelInfo::minimal("m", "one"))
        .expect("test result");
    r.insert(ModelInfo::minimal("m", "two"))
        .expect("test result");
    let err = resolve_model(&r, &ModelTarget::id("m")).expect_err("expected error");
    match err {
        KernelError::AmbiguousModel(id) => assert_eq!(id.as_str(), "m"),
        other => panic!("unexpected: {other:?}"),
    }
}

#[test]
fn resolve_target_maps_unknown_to_kernel_error() {
    let r = ModelRegistry::new();
    let err = resolve_model_target(&r, &ModelTarget::new("p", "m")).expect_err("expected error");
    match err {
        KernelError::UnknownModelTarget(target) => {
            assert_eq!(target.provider(), Some("p"));
            assert_eq!(target.as_str(), "m");
        }
        other => panic!("unexpected: {other:?}"),
    }
}

#[tokio::test]
async fn resolve_with_overrides_uses_config_default() {
    let r = registry_with(&["default"]);
    let store = TestGlobalStore::default();

    let resolved = resolve_model_with_overrides(&r, None, &store, None, &ModelId::new("default"))
        .await
        .expect("test result");

    assert_eq!(resolved.info.id.as_str(), "default");
    assert_eq!(resolved.source, ResolvedSource::ConfigDefault);
}

#[tokio::test]
async fn resolve_with_overrides_uses_global_override() {
    let r = registry_with(&["default", "global"]);
    let store = TestGlobalStore {
        model: Some(ModelId::new("global")),
        fail: false,
    };

    let resolved = resolve_model_with_overrides(&r, None, &store, None, &ModelId::new("default"))
        .await
        .expect("test result");

    assert_eq!(resolved.info.id.as_str(), "global");
    assert_eq!(resolved.source, ResolvedSource::GlobalOverride);
}

#[tokio::test]
async fn resolve_with_overrides_uses_session_before_global() {
    let r = registry_with(&["default", "global", "session"]);
    let store = TestGlobalStore {
        model: Some(ModelId::new("global")),
        fail: false,
    };

    let resolved = resolve_model_with_overrides(
        &r,
        Some(&ModelId::new("session")),
        &store,
        None,
        &ModelId::new("default"),
    )
    .await
    .expect("test result");

    assert_eq!(resolved.info.id.as_str(), "session");
    assert_eq!(resolved.source, ResolvedSource::SessionOverride);
}

#[tokio::test]
async fn resolve_with_overrides_uses_explicit_before_session() {
    let r = registry_with(&["default", "global", "session", "explicit"]);
    let store = TestGlobalStore {
        model: Some(ModelId::new("global")),
        fail: false,
    };

    let resolved = resolve_model_with_overrides(
        &r,
        Some(&ModelId::new("session")),
        &store,
        Some(&ModelId::new("explicit")),
        &ModelId::new("default"),
    )
    .await
    .expect("test result");

    assert_eq!(resolved.info.id.as_str(), "explicit");
    assert_eq!(resolved.source, ResolvedSource::Explicit);
}

#[tokio::test]
async fn resolve_with_overrides_fails_closed_for_unknown_session() {
    let r = registry_with(&["default"]);
    let store = TestGlobalStore::default();

    let err = resolve_model_with_overrides(
        &r,
        Some(&ModelId::new("missing")),
        &store,
        None,
        &ModelId::new("default"),
    )
    .await
    .expect_err("expected error");

    assert!(matches!(
        err,
        ResolveModelError::UnknownOverride { scope: "session", model }
            if model.as_str() == "missing"
    ));
}

#[tokio::test]
async fn resolve_with_overrides_fails_closed_for_unknown_global() {
    let r = registry_with(&["default"]);
    let store = TestGlobalStore {
        model: Some(ModelId::new("missing")),
        fail: false,
    };

    let err = resolve_model_with_overrides(&r, None, &store, None, &ModelId::new("default"))
        .await
        .expect_err("expected error");

    assert!(matches!(
        err,
        ResolveModelError::UnknownOverride { scope: "global", model }
            if model.as_str() == "missing"
    ));
}

#[tokio::test]
async fn resolve_with_overrides_checks_global_when_session_absent() {
    let r = registry_with(&["default"]);
    let store = TestGlobalStore {
        model: Some(ModelId::new("missing")),
        fail: false,
    };

    let err = resolve_model_with_overrides(&r, None, &store, None, &ModelId::new("default"))
        .await
        .expect_err("expected error");

    assert!(matches!(
        err,
        ResolveModelError::UnknownOverride {
            scope: "global",
            ..
        }
    ));
}

#[test]
fn override_store_error_maps_to_kernel_error() {
    let err = ResolveModelError::OverrideStore(ModelOverrideStoreError::backend("offline"));
    let mapped = map_resolve_error(err);

    assert!(matches!(
        mapped,
        KernelError::ModelOverrideStore { reason } if reason.contains("offline")
    ));
}

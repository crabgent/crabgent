use crabgent_core::{AllowAllPolicy, BuildError, Kernel, ModelInfo};
use crabgent_test_support::StubProvider;

fn provider(name: &'static str, models: &[&str]) -> StubProvider {
    StubProvider::new().with_name(name).with_models(
        models
            .iter()
            .map(|model| ModelInfo::minimal(*model, name))
            .collect(),
    )
}

fn expect_build_error(result: Result<Kernel, BuildError>) -> BuildError {
    result.err().expect("invalid provider catalog should fail")
}

#[test]
fn try_build_succeeds_on_valid_provider_catalog() {
    let kernel = Kernel::builder()
        .provider(provider("primary", &["model-a"]))
        .policy(AllowAllPolicy)
        .try_build()
        .expect("valid provider catalog should build");

    assert_eq!(kernel.provider_name(), "primary");
    assert_eq!(kernel.models().len(), 1);
}

#[test]
fn try_build_returns_err_on_duplicate_provider() {
    let err = expect_build_error(
        Kernel::builder()
            .provider(provider("dup", &["model-a"]))
            .provider(provider("dup", &["model-b"]))
            .policy(AllowAllPolicy)
            .try_build(),
    );

    assert_eq!(
        err,
        BuildError::DuplicateProvider {
            provider_name: "dup".into()
        }
    );
}

#[test]
fn try_build_allows_duplicate_model_ids_across_providers() {
    let kernel = Kernel::builder()
        .provider(provider("primary", &["claude-3"]))
        .provider(provider("fallback", &["claude-3"]))
        .policy(AllowAllPolicy)
        .try_build()
        .expect("provider-qualified duplicate model ids should build");

    assert_eq!(kernel.models().len(), 2);
}

#[test]
fn try_build_returns_err_on_provider_mismatch() {
    let err = expect_build_error(
        Kernel::builder()
            .provider(
                StubProvider::new()
                    .with_name("actual")
                    .with_models(vec![ModelInfo::minimal("mismatch", "other")]),
            )
            .policy(AllowAllPolicy)
            .try_build(),
    );

    assert_eq!(
        err,
        BuildError::ProviderMismatch {
            provider: "actual".into(),
            model: "mismatch".into()
        }
    );
}

#[test]
#[should_panic(expected = "duplicate provider name: dup")]
fn build_panics_for_test_only_invalid_provider_catalog() {
    let _kernel = Kernel::builder()
        .provider(provider("dup", &["model-a"]))
        .provider(provider("dup", &["model-b"]))
        .policy(AllowAllPolicy)
        .build();
}

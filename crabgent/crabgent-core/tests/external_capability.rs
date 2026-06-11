use crabgent_core::{ModelCapability, ModelInfo};

struct TestCapability {
    data: String,
}

impl ModelCapability for TestCapability {}

struct OtherCapability;

impl ModelCapability for OtherCapability {}

#[test]
fn external_capability_discovery() {
    let info =
        ModelInfo::minimal("m", "external").with_extension(TestCapability { data: "ok".into() });

    let capability = info
        .capability::<TestCapability>()
        .expect("capability registered");

    assert_eq!(capability.data, "ok");
}

#[test]
fn external_capability_wrong_type_returns_none() {
    let info =
        ModelInfo::minimal("m", "external").with_extension(TestCapability { data: "ok".into() });

    assert!(info.capability::<OtherCapability>().is_none());
}

use super::*;

fn conv_key() -> ConvKey {
    ConvKey("slack".to_owned(), "slack:T1/D1".to_owned())
}

#[tokio::test]
async fn cancel_conv_fires_token_and_clears_entry() {
    let lifecycle = InboxLifecycle::new(DEFAULT_MAX_CONCURRENT_RUNS);
    let key = conv_key();
    let ClaimResult::Spawned { cancel, .. } =
        lifecycle.try_claim_conv(key.clone(), RunId::new()).await
    else {
        panic!("first claim should spawn");
    };

    assert!(!cancel.is_cancelled());
    assert!(lifecycle.cancel_conv(&key).await);
    assert!(cancel.is_cancelled());
    let next_run_id = RunId::new();
    assert!(matches!(
        lifecycle
            .try_claim_conv(key.clone(), next_run_id.clone())
            .await,
        ClaimResult::Spawned { .. }
    ));
    lifecycle.release_conv(&key, &next_run_id).await;
}

#[tokio::test]
async fn cancel_conv_unknown_key_returns_false() {
    let lifecycle = InboxLifecycle::new(DEFAULT_MAX_CONCURRENT_RUNS);
    assert!(!lifecycle.cancel_conv(&conv_key()).await);
}

#[tokio::test]
async fn cancel_conv_does_not_fire_global() {
    let lifecycle = InboxLifecycle::new(DEFAULT_MAX_CONCURRENT_RUNS);
    let key = conv_key();
    assert!(matches!(
        lifecycle.try_claim_conv(key.clone(), RunId::new()).await,
        ClaimResult::Spawned { .. }
    ));

    assert!(lifecycle.cancel_conv(&key).await);
    assert!(!lifecycle.is_shutdown());
}

#[tokio::test]
async fn try_claim_conv_returns_empty_reason_cell() {
    let lifecycle = InboxLifecycle::new(DEFAULT_MAX_CONCURRENT_RUNS);
    let ClaimResult::Spawned { cancel_reason, .. } =
        lifecycle.try_claim_conv(conv_key(), RunId::new()).await
    else {
        panic!("first claim should spawn");
    };
    assert!(cancel_reason.get().is_none());
}

#[tokio::test]
async fn cancel_conv_propagates_stop_pattern_to_caller_cell() {
    let lifecycle = InboxLifecycle::new(DEFAULT_MAX_CONCURRENT_RUNS);
    let key = conv_key();
    let ClaimResult::Spawned {
        cancel,
        cancel_reason,
    } = lifecycle.try_claim_conv(key.clone(), RunId::new()).await
    else {
        panic!("first claim should spawn");
    };
    assert!(cancel_reason.get().is_none());

    assert!(lifecycle.cancel_conv(&key).await);

    assert!(cancel.is_cancelled());
    assert_eq!(
        cancel_reason.get().copied(),
        Some(CancelReason::StopPattern)
    );
}

#[tokio::test]
async fn cancel_conv_on_unknown_key_does_not_touch_reason_of_other_conv() {
    let lifecycle = InboxLifecycle::new(DEFAULT_MAX_CONCURRENT_RUNS);
    let key = conv_key();
    let ClaimResult::Spawned { cancel_reason, .. } =
        lifecycle.try_claim_conv(key.clone(), RunId::new()).await
    else {
        panic!("first claim should spawn");
    };

    let other = ConvKey("slack".to_owned(), "slack:T1/D2".to_owned());
    assert!(!lifecycle.cancel_conv(&other).await);
    assert!(cancel_reason.get().is_none());

    lifecycle.release_conv(&key, &RunId::new()).await;
}

#[tokio::test]
async fn shutdown_with_grace_honors_caller_supplied_grace() {
    use std::time::Instant;
    use tokio::time::sleep;

    let lifecycle = InboxLifecycle::new_with_grace(1, Duration::from_secs(10));
    // Spawn a task that ignores the lifecycle cancel and sleeps 60s. The
    // drain timeout decides how long shutdown blocks before abort_all.
    lifecycle
        .spawn_run(RunId::new(), async move {
            sleep(Duration::from_mins(1)).await;
            Ok(())
        })
        .await
        .expect("spawn within max_concurrent");

    let start = Instant::now();
    lifecycle
        .shutdown_with_grace(Duration::from_millis(50))
        .await;
    let elapsed = start.elapsed();

    assert!(
        elapsed < Duration::from_secs(1),
        "caller grace=50ms must trigger abort before lifecycle-default 10s, elapsed={elapsed:?}"
    );
    assert!(
        elapsed >= Duration::from_millis(50),
        "caller grace must be respected (no early abort), elapsed={elapsed:?}"
    );
}

#[tokio::test]
async fn shutdown_with_grace_zero_falls_back_to_configured_grace() {
    use std::time::Instant;
    use tokio::time::sleep;

    let lifecycle = InboxLifecycle::new_with_grace(1, Duration::from_millis(80));
    lifecycle
        .spawn_run(RunId::new(), async move {
            sleep(Duration::from_mins(1)).await;
            Ok(())
        })
        .await
        .expect("spawn within max_concurrent");

    let start = Instant::now();
    lifecycle.shutdown_with_grace(Duration::ZERO).await;
    let elapsed = start.elapsed();

    assert!(
        elapsed >= Duration::from_millis(80),
        "Duration::ZERO sentinel must fall back to configured 80ms grace, elapsed={elapsed:?}"
    );
    assert!(
        elapsed < Duration::from_secs(1),
        "configured grace must bound shutdown, elapsed={elapsed:?}"
    );
}

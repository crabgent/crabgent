//! Classification matrix for [`spawn::classify_outcome`]: pause flag,
//! force-pause attribution, user-cancel precedence, and timeout
//! precedence.

use super::spawn::TaskFinal;
use super::*;

use crate::DrainOutcome;

fn outcome(final_text: Option<&str>, error: Option<&str>, paused: bool) -> DrainOutcome {
    DrainOutcome {
        final_text: final_text.map(str::to_owned),
        error: error.map(str::to_owned),
        paused,
    }
}

#[test]
fn classify_success_outcome_yields_done() {
    let o = outcome(Some("ok"), None, false);
    assert_eq!(spawn::classify_outcome(&o, false, None), TaskFinal::Done);
}

#[test]
fn classify_error_outcome_yields_failed() {
    let o = outcome(None, Some("boom"), false);
    assert_eq!(
        spawn::classify_outcome(&o, false, None),
        TaskFinal::Failed("boom".into())
    );
}

#[test]
fn classify_clean_close_without_final_yields_failed() {
    // A stream that closed cleanly with neither a final text nor an error
    // (e.g. MaxTurnsExceeded dropped the channel) must not be reported Done.
    let o = outcome(None, None, false);
    assert_eq!(
        spawn::classify_outcome(&o, false, None),
        TaskFinal::Failed("run ended without final event".into())
    );
}

#[test]
fn classify_paused_flag_yields_shutdown_pause() {
    let o = outcome(None, None, true);
    assert_eq!(
        spawn::classify_outcome(&o, false, None),
        TaskFinal::Paused(crabgent_store::TaskPauseCause::Shutdown)
    );
}

#[test]
fn classify_cancel_during_pause_window_yields_forced_pause() {
    let o = outcome(None, Some(CANCELLED_MESSAGE), false);
    assert_eq!(
        spawn::classify_outcome(&o, true, Some(crabgent_core::CancelReason::Shutdown)),
        TaskFinal::Paused(crabgent_store::TaskPauseCause::Forced)
    );
    // Without a pause window, the same cancel stays a plain failure.
    assert_eq!(
        spawn::classify_outcome(&o, false, None),
        TaskFinal::Failed(CANCELLED_MESSAGE.into())
    );
}

#[test]
fn classify_user_stop_pattern_beats_paused_flag() {
    // A user cancel stamped before classification wins even when the run
    // exited at a clean pause boundary: cancelled work never resurrects.
    let o = outcome(None, None, true);
    assert_eq!(
        spawn::classify_outcome(&o, true, Some(crabgent_core::CancelReason::StopPattern)),
        TaskFinal::Failed(CANCELLED_MESSAGE.into())
    );
}

#[test]
fn classify_user_stop_pattern_beats_pause_window() {
    let o = outcome(None, Some(CANCELLED_MESSAGE), false);
    assert_eq!(
        spawn::classify_outcome(&o, true, Some(crabgent_core::CancelReason::StopPattern)),
        TaskFinal::Failed(CANCELLED_MESSAGE.into())
    );
}

#[test]
fn classify_timeout_beats_pause_window() {
    let o = outcome(None, Some(TIMEOUT_MESSAGE), false);
    assert_eq!(
        spawn::classify_outcome(&o, true, None),
        TaskFinal::Failed(TIMEOUT_MESSAGE.into())
    );
}

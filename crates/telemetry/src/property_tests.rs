use crate::{
    EventKind, EventLog, PingRecordOutcome, PingTracker, RateWindow, TelemetryHub,
    EVENT_LOG_CAPACITY, MAX_OUTSTANDING_PINGS,
};
use proptest::prelude::*;

proptest! {
    #[test]
    fn telemetry_operations_are_bounded_for_arbitrary_timestamps_and_nonces(
        operations in prop::collection::vec(
            (any::<u64>(), any::<u64>(), any::<u64>(), any::<u8>(), any::<f64>(), any::<f64>()),
            0..128,
        )
    ) {
        let mut rates = RateWindow::default();
        let mut pings = PingTracker::new();
        let mut log = EventLog::new();
        let mut hub = TelemetryHub::new();
        let mut prior_event_id = 0;
        for (now_us, bytes, nonce, tag, packet_loss, frame_loss) in operations {
            let _ = rates.record(now_us, u64::from(tag), bytes);
            let _ = rates.snapshot(now_us);
            let outcome = pings.record_ping(nonce, now_us.wrapping_add(1), now_us);
            if tag & 1 == 0 {
                let _ = pings.match_pong(nonce, now_us.wrapping_add(1), now_us.saturating_add(10));
            }
            let _ = pings.expire(now_us, u64::from(tag).saturating_add(1));
            prop_assert!(pings.snapshot().outstanding <= MAX_OUTSTANDING_PINGS);
            prop_assert!(matches!(outcome, PingRecordOutcome::Recorded | PingRecordOutcome::DroppedOldest | PingRecordOutcome::DuplicateNonce));

            let event = log.push(now_us, EventKind::PacketLossEvent, format!("tag={tag}")).expect("process event id");
            prop_assert!(event.id > prior_event_id);
            prior_event_id = event.id;
            prop_assert!(log.len() <= EVENT_LOG_CAPACITY);

            hub.record_rtt(now_us, bytes);
            hub.record_frame(now_us, tag & 2 != 0);
            hub.record_bytes(now_us, bytes);
            hub.record_loss(now_us, packet_loss, frame_loss);
            hub.push_event(now_us, EventKind::StreamReset, "property").expect("process event id");
            let snapshot = hub.snapshot(now_us);
            prop_assert!(snapshot.session.loss_fraction.is_finite());
            prop_assert!(snapshot.session.frame_loss_fraction.is_finite());
            prop_assert!(snapshot.session.loss_fraction >= 0.0 && snapshot.session.loss_fraction <= 1.0);
            prop_assert!(snapshot.session.frame_loss_fraction >= 0.0 && snapshot.session.frame_loss_fraction <= 1.0);
            prop_assert!(snapshot.session.fps.is_finite());
            prop_assert!(snapshot.session.srtt_us.is_none_or(f64::is_finite));
            prop_assert!(hub.ping_snapshot().outstanding <= MAX_OUTSTANDING_PINGS);
            prop_assert!(hub.events_since(0).events().len() <= EVENT_LOG_CAPACITY);
        }
        let final_rates = rates.snapshot(u64::MAX);
        prop_assert!(final_rates.events_per_second.is_finite());
        prop_assert!(final_rates.bytes_per_second.is_finite());
        prop_assert!(log.snapshot().events().len() <= EVENT_LOG_CAPACITY);
    }

    #[test]
    fn ping_tracker_never_exceeds_sixteen_across_generated_ping_streams(
        pings_in in prop::collection::vec((any::<u64>(), any::<u64>(), any::<bool>()), 0..512)
    ) {
        let mut tracker = PingTracker::new();
        for (nonce, now_us, should_expire) in pings_in {
            let _ = tracker.record_ping(nonce, nonce.rotate_left(13), now_us);
            if should_expire {
                let _ = tracker.expire(now_us.saturating_add(1000), 100);
            }
            prop_assert!(tracker.snapshot().outstanding <= MAX_OUTSTANDING_PINGS);
        }
    }
}

#[test]
fn observed_frame_loss_fraction_uses_rolling_frame_counts() {
    let mut hub = TelemetryHub::new();
    hub.record_frame(10, false);
    hub.record_frame(20, true);
    let snapshot = hub.snapshot(30);
    assert_eq!(snapshot.session.frame_loss_fraction, 0.5);
}

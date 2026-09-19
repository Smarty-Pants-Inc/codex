use super::reserve_observation_capacity;
use super::tokens_remaining;
use pretty_assertions::assert_eq;

#[test]
fn reservation_reduces_both_total_and_body_after_prefix_capacity() {
    for used in [0, 100, 500] {
        let (scope, full) = reserve_observation_capacity(
            /*auto_compact_limit*/ Some(1000),
            /*full_context_limit*/ Some(2000),
            /*reserve*/ 600,
        );
        assert_eq!((scope, full), (Some(400), Some(1400)));
        assert_eq!(tokens_remaining(scope, used), Some((400_i64 - used).max(0)));
        assert_eq!(tokens_remaining(full, used), Some(1400 - used));
    }
}

#[test]
fn model_shrink_or_unknown_capacity_does_not_spend_the_reserve() {
    for full in [None, Some(100), Some(600)] {
        let limits = reserve_observation_capacity(
            /*auto_compact_limit*/ Some(100),
            full,
            /*reserve*/ 600,
        );
        assert_eq!(limits, (Some(0), Some(0)));
    }
    assert_eq!(
        reserve_observation_capacity(
            /*auto_compact_limit*/ None,
            /*full_context_limit*/ Some(1000),
            /*reserve*/ 600,
        ),
        (None, Some(400)),
    );
}

#[test]
fn disabled_observations_preserve_existing_limit_semantics() {
    for limits in [
        (None, None),
        (Some(100), None),
        (None, Some(200)),
        (Some(100), Some(200)),
    ] {
        assert_eq!(
            reserve_observation_capacity(limits.0, limits.1, /*reserve*/ 0),
            limits
        );
    }
}

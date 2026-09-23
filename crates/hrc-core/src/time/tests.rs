use super::*;

#[test]
fn a_timestamp_round_trips_through_both_directions() {
    for stamp in [
        "1970-01-01T00:00:00Z",
        "2000-02-29T12:34:56Z",
        "2026-09-13T07:08:09Z",
        "2100-03-01T00:00:00Z",
    ] {
        let seconds = epoch_seconds(stamp).expect(stamp);
        assert_eq!(rfc3339_from(seconds), stamp);
    }
}

#[test]
fn only_the_timestamp_shape_this_project_emits_is_parsed() {
    // Accepting offsets and fractions nothing here produces would mean
    // quietly mis-parsing one of them later.
    for bad in [
        "2026-09-13T00:00:00+02:00",
        "2026-09-13T00:00:00.500Z",
        "2026-09-13 00:00:00Z",
        "2026-09-13",
    ] {
        assert!(epoch_seconds(bad).is_none(), "{bad:?} was parsed");
    }
}

#[test]
fn an_impossible_month_or_day_is_refused_rather_than_carried() {
    // The copy in the inbox view checked this and the copy in the CLI did
    // not. A month of 13 carried into the arithmetic produces a plausible
    // wrong date, which is worse than no date.
    for bad in [
        "2026-13-01T00:00:00Z",
        "2026-00-01T00:00:00Z",
        "2026-01-32T00:00:00Z",
        "2026-01-00T00:00:00Z",
    ] {
        assert!(epoch_seconds(bad).is_none(), "{bad:?} was parsed");
    }
}

#[test]
fn a_leap_day_and_a_century_are_placed_correctly() {
    assert_eq!(
        epoch_seconds("2028-02-29T00:00:00Z").unwrap()
            - epoch_seconds("2028-02-28T00:00:00Z").unwrap(),
        86_400
    );
    // 2100 is not a leap year; 2000 was.
    assert_eq!(
        epoch_seconds("2100-03-01T00:00:00Z").unwrap()
            - epoch_seconds("2100-02-28T00:00:00Z").unwrap(),
        86_400
    );
    assert_eq!(
        epoch_seconds("2000-03-01T00:00:00Z").unwrap()
            - epoch_seconds("2000-02-28T00:00:00Z").unwrap(),
        2 * 86_400
    );
}

#[test]
fn a_short_duration_is_coarse_at_each_boundary() {
    assert_eq!(short_duration(0), "0s");
    assert_eq!(short_duration(59), "59s");
    assert_eq!(short_duration(60), "1m");
    assert_eq!(short_duration(3599), "59m");
    assert_eq!(short_duration(3600), "1h");
    assert_eq!(short_duration(86_399), "23h");
    assert_eq!(short_duration(86_400), "1d");
}

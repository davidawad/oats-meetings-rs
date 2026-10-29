//! ICS parsing and recurrence expansion against the synthetic fixtures in tests/fixtures/ics.
//! Zones covered: America/New_York (spring-forward and fall-back), Europe/London (BST ends),
//! Australia/Sydney (spring-forward gap), Asia/Tokyo, UTC and floating times.

use std::path::Path;

use chrono::{DateTime, Utc};
use chrono_tz::Tz;
use oats_core::ics::parse::{expand, parse_feed, ParseOpts};
use serde_json::Value;

fn opts() -> ParseOpts {
    ParseOpts {
        default_tz: Tz::UTC,
        tz_fallback: false,
        skip_bad: false,
        show_cancelled: false,
        include_all_day: true,
    }
}

fn fixture(name: &str) -> String {
    let p = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../tests/fixtures/ics")
        .join(name);
    std::fs::read_to_string(&p).unwrap_or_else(|e| panic!("{}: {e}", p.display()))
}

fn t(s: &str) -> DateTime<Utc> {
    DateTime::parse_from_rfc3339(s).unwrap().with_timezone(&Utc)
}

fn run(name: &str, o: &ParseOpts, from: &str, to: &str) -> Vec<Value> {
    let p = parse_feed(&fixture(name), o).unwrap_or_else(|e| panic!("{name}: {e}"));
    expand("f", &p, o, 500, t(from), t(to)).unwrap()
}

fn starts(v: &[Value]) -> Vec<&str> {
    v.iter().map(|e| e["start"].as_str().unwrap()).collect()
}

fn summaries(v: &[Value]) -> Vec<&str> {
    v.iter().map(|e| e["summary"].as_str().unwrap()).collect()
}

#[test]
fn new_york_weekly_across_spring_forward_with_exdate_and_override() {
    let v = run(
        "weekly-ny.ics",
        &opts(),
        "2026-03-01T00:00:00Z",
        "2026-05-01T00:00:00Z",
    );
    // Mar 2 is EST (UTC-5); from Mar 8 the same 09:00 wall time is EDT (UTC-4). Mar 16 is
    // EXDATEd; Mar 23 is replaced by the 11:00 override; COUNT=8 ends the series on Apr 20.
    assert_eq!(
        starts(&v),
        [
            "2026-03-02T14:00:00Z",
            "2026-03-09T13:00:00Z",
            "2026-03-23T15:00:00Z",
            "2026-03-30T13:00:00Z",
            "2026-04-06T13:00:00Z",
            "2026-04-13T13:00:00Z",
            "2026-04-20T13:00:00Z",
        ]
    );
    assert_eq!(v[2]["summary"], "Team standup (moved)");
    assert_eq!(v[2]["override"], true);
    assert_eq!(v[0]["summary"], "Team standup");
    assert_eq!(v[0]["location"], "Room 4, floor 2");
    assert_eq!(v[0]["end"], "2026-03-02T14:30:00Z");
    assert_eq!(v[0]["tzid"], "America/New_York");
    assert_eq!(v[0]["local_start"], "2026-03-02T09:00:00");
    assert_eq!(v[0]["recurring"], true);
    assert_eq!(v[1]["local_start"], "2026-03-09T09:00:00");
}

#[test]
fn window_selects_overlapping_occurrences_only() {
    let v = run(
        "weekly-ny.ics",
        &opts(),
        "2026-03-09T13:15:00Z",
        "2026-03-09T13:20:00Z",
    );
    assert_eq!(starts(&v), ["2026-03-09T13:00:00Z"]); // already started, still running
    let v = run(
        "weekly-ny.ics",
        &opts(),
        "2026-03-09T13:30:00Z",
        "2026-03-23T00:00:00Z",
    );
    assert!(v.is_empty()); // ends exactly at the window start; the 16th is excluded
}

#[test]
fn london_daily_across_end_of_bst_with_utc_until() {
    let v = run(
        "london-bst-end.ics",
        &opts(),
        "2026-10-01T00:00:00Z",
        "2026-12-01T00:00:00Z",
    );
    // BST (UTC+1) until 01:00Z on Oct 25, then GMT. UNTIL=2026-10-27T23:00Z keeps Oct 27 only.
    assert_eq!(
        starts(&v),
        [
            "2026-10-22T09:00:00Z",
            "2026-10-23T09:00:00Z",
            "2026-10-24T09:00:00Z",
            "2026-10-25T10:00:00Z",
            "2026-10-26T10:00:00Z",
            "2026-10-27T10:00:00Z",
        ]
    );
    assert!(v
        .iter()
        .all(|e| e["local_start"].as_str().unwrap().ends_with("T10:00:00")));
}

#[test]
fn sydney_weekly_through_a_nonexistent_wall_time() {
    let v = run(
        "sydney-dst-gap.ics",
        &opts(),
        "2026-09-01T00:00:00Z",
        "2026-12-01T00:00:00Z",
    );
    // Oct 4 02:30 does not exist in Sydney (02:00 -> 03:00). It is read with the offset from
    // before the gap (+10), i.e. 16:30Z the day before, which is 03:30 AEDT.
    assert_eq!(
        starts(&v),
        [
            "2026-09-26T16:30:00Z",
            "2026-10-03T16:30:00Z",
            "2026-10-10T15:30:00Z"
        ]
    );
    assert_eq!(v[1]["end"], "2026-10-03T17:30:00Z");
}

#[test]
fn new_york_weekly_through_an_ambiguous_wall_time_takes_the_first() {
    let v = run(
        "ny-fall-back.ics",
        &opts(),
        "2026-10-01T00:00:00Z",
        "2026-12-01T00:00:00Z",
    );
    // Nov 1 01:30 happens twice; the first (EDT, UTC-4) is used. Nov 8 is plain EST.
    assert_eq!(
        starts(&v),
        [
            "2026-10-25T05:30:00Z",
            "2026-11-01T05:30:00Z",
            "2026-11-08T06:30:00Z"
        ]
    );
    assert_eq!(v[0]["end"], "2026-10-25T06:15:00Z");
}

#[test]
fn tokyo_utc_and_floating_zones() {
    let v = run(
        "zones-misc.ics",
        &opts(),
        "2026-07-15T00:00:00Z",
        "2026-07-16T00:00:00Z",
    );
    assert_eq!(summaries(&v), ["Floating", "Tokyo", "UTC"]);
    assert_eq!(
        starts(&v),
        [
            "2026-07-15T08:00:00Z",
            "2026-07-15T09:00:00Z",
            "2026-07-15T12:00:00Z"
        ]
    );
    assert_eq!(v[1]["tzid"], "Asia/Tokyo");
}

#[test]
fn floating_times_follow_default_tz_for_every_value() {
    for (tz, want) in [
        (Tz::UTC, "2026-07-15T08:00:00Z"),
        (Tz::Europe__Berlin, "2026-07-15T06:00:00Z"),
        (Tz::America__Los_Angeles, "2026-07-15T15:00:00Z"),
        (Tz::Asia__Kolkata, "2026-07-15T02:30:00Z"),
    ] {
        let o = ParseOpts {
            default_tz: tz,
            ..opts()
        };
        let v = run(
            "zones-misc.ics",
            &o,
            "2026-07-14T00:00:00Z",
            "2026-07-17T00:00:00Z",
        );
        let f = v.iter().find(|e| e["summary"] == "Floating").unwrap();
        assert_eq!(f["start"], want, "{tz}");
        assert_eq!(f["tzid"], tz.name());
    }
}

#[test]
fn all_day_events_single_multi_day_yearly_until_and_weekly_exdate() {
    let v = run(
        "all-day.ics",
        &opts(),
        "2026-03-01T00:00:00Z",
        "2026-04-01T00:00:00Z",
    );
    assert_eq!(v.len(), 1);
    assert_eq!(v[0]["summary"], "Conference");
    assert_eq!(v[0]["all_day"], true);
    assert_eq!(v[0]["start_date"], "2026-03-10");
    assert_eq!(v[0]["end_date"], "2026-03-12"); // exclusive, two days
    assert_eq!(v[0]["start"], "2026-03-10T00:00:00Z");
    assert_eq!(v[0]["end"], "2026-03-12T00:00:00Z");

    let v = run(
        "all-day.ics",
        &opts(),
        "2026-06-01T00:00:00Z",
        "2026-07-01T00:00:00Z",
    );
    let days: Vec<&str> = v
        .iter()
        .map(|e| e["start_date"].as_str().unwrap())
        .collect();
    assert_eq!(
        days,
        ["2026-06-01", "2026-06-05", "2026-06-15", "2026-06-22"]
    ); // 8th is EXDATEd
    assert_eq!(v[1]["summary"], "Birthday");
    assert_eq!(v[1]["end_date"], "2026-06-06");

    // UNTIL=20290101 allows June 2026, 2027, 2028 and nothing after.
    let v = run(
        "all-day.ics",
        &opts(),
        "2026-06-05T00:00:00Z",
        "2031-01-01T00:00:00Z",
    );
    let b: Vec<&str> = v
        .iter()
        .filter(|e| e["summary"] == "Birthday")
        .map(|e| e["start_date"].as_str().unwrap())
        .collect();
    assert_eq!(b, ["2026-06-05", "2027-06-05", "2028-06-05"]);
}

#[test]
fn all_day_bounds_follow_default_tz() {
    let o = ParseOpts {
        default_tz: Tz::America__Los_Angeles,
        ..opts()
    };
    let v = run(
        "all-day.ics",
        &o,
        "2026-06-04T00:00:00Z",
        "2026-06-07T00:00:00Z",
    );
    let b = v.iter().find(|e| e["summary"] == "Birthday").unwrap();
    assert_eq!(b["start"], "2026-06-05T07:00:00Z"); // midnight PDT
    assert_eq!(b["end"], "2026-06-06T07:00:00Z");
    assert_eq!(b["start_date"], "2026-06-05");
}

#[test]
fn all_day_can_be_excluded() {
    let o = ParseOpts {
        include_all_day: false,
        ..opts()
    };
    let v = run(
        "all-day.ics",
        &o,
        "2026-01-01T00:00:00Z",
        "2030-01-01T00:00:00Z",
    );
    assert!(v.is_empty());
}

#[test]
fn folding_escapes_duration_rdate_cancelled_and_zero_length() {
    let v = run(
        "misc.ics",
        &opts(),
        "2026-07-20T00:00:00Z",
        "2026-07-27T00:00:00Z",
    );
    assert_eq!(
        v[0]["summary"],
        "Quarterly planning with a really long title that has to be folded across two lines; semicolon\nsecond line"
    );
    let dur = v.iter().find(|e| e["uid"] == "dur@fixture").unwrap();
    assert_eq!(dur["start"], "2026-07-20T14:00:00Z");
    assert_eq!(dur["end"], "2026-07-20T15:30:00Z");
    let rd: Vec<&str> = v
        .iter()
        .filter(|e| e["uid"] == "rd@fixture")
        .map(|e| e["start"].as_str().unwrap())
        .collect();
    assert_eq!(
        rd,
        [
            "2026-07-21T10:00:00Z",
            "2026-07-23T10:00:00Z",
            "2026-07-25T10:00:00Z"
        ]
    );
    assert!(
        v.iter().all(|e| e["uid"] != "cx@fixture"),
        "cancelled is hidden by default"
    );
    let ms = v.iter().find(|e| e["uid"] == "ms@fixture").unwrap();
    assert_eq!(ms["start"], ms["end"]);
}

#[test]
fn cancelled_events_can_be_shown() {
    let o = ParseOpts {
        show_cancelled: true,
        ..opts()
    };
    let v = run(
        "misc.ics",
        &o,
        "2026-07-22T00:00:00Z",
        "2026-07-23T00:00:00Z",
    );
    assert_eq!(v.len(), 1);
    assert_eq!(v[0]["status"], "CANCELLED");
}

#[test]
fn lf_only_line_endings_parse() {
    let v = run(
        "lf-only.ics",
        &opts(),
        "2026-08-01T00:00:00Z",
        "2026-08-02T00:00:00Z",
    );
    assert_eq!(summaries(&v), ["LF line endings"]);
}

#[test]
fn max_occurrences_caps_a_recurring_event() {
    let o = opts();
    let p = parse_feed(&fixture("london-bst-end.ics"), &o).unwrap();
    let v = expand(
        "f",
        &p,
        &o,
        2,
        t("2026-10-01T00:00:00Z"),
        t("2026-12-01T00:00:00Z"),
    )
    .unwrap();
    assert_eq!(v.len(), 2);
}

#[test]
fn malformed_feeds_have_stable_codes() {
    let o = opts();
    for (file, code) in [
        ("not-a-calendar.html", "ics_not_calendar"),
        ("truncated.ics", "ics_parse_error"),
        ("no-dtstart.ics", "ics_bad_event"),
        ("bad-date.ics", "ics_bad_event"),
        ("bad-tz.ics", "ics_bad_tz"),
        ("bad-rrule.ics", "ics_bad_rrule"),
        ("mixed.ics", "ics_bad_event"),
    ] {
        let e = parse_feed(&fixture(file), &o).unwrap_err();
        assert_eq!(e.code, code, "{file}: {e}");
    }
    assert_eq!(parse_feed("", &o).unwrap_err().code, "ics_not_calendar");
}

#[test]
fn skip_drops_bad_events_and_counts_them() {
    let o = ParseOpts {
        skip_bad: true,
        ..opts()
    };
    let p = parse_feed(&fixture("mixed.ics"), &o).unwrap();
    assert_eq!((p.events.len(), p.skipped), (1, 1));
    let p = parse_feed(&fixture("bad-rrule.ics"), &o).unwrap();
    assert_eq!((p.events.len(), p.skipped), (0, 1));
}

#[test]
fn unknown_tzid_falls_back_to_default_tz_when_configured() {
    let o = ParseOpts {
        tz_fallback: true,
        default_tz: Tz::Asia__Tokyo,
        ..opts()
    };
    let v = run(
        "bad-tz.ics",
        &o,
        "2026-07-14T00:00:00Z",
        "2026-07-16T00:00:00Z",
    );
    assert_eq!(starts(&v), ["2026-07-15T01:00:00Z"]); // 10:00 JST
}

//! `calendar.ending` and `calendar.ended` (dot-4uzgol.111.7, docs/design/hooks.md): the timer
//! that says when a recording matched to a calendar event nears and passes the event's scheduled
//! end. `tick` is pure (the clock is its argument): the engine calls it with the wall clock from
//! the recording's watcher, every `record.auto_check`, and the tests with a fake one.
//!
//! - **`calendar.ending`** happens per hook: a hook is due once the time left is at most its
//!   `match.before_end` (default `app.hook_before_end`), and only while the end is still ahead.
//!   Each hook is due once per recording; one tick fires the event once for every hook that came
//!   due on it, and a hook not due yet gets no decision (the event did not happen for it).
//! - **`calendar.ended`** happens once, at the first tick at or past the end, and only when an
//!   earlier tick saw the end still ahead (the end passed while recording, not before it began).
//!   `overrun_ms` is how far past the end that tick was.
//!
//! A tick that jumps over a hook's window (the machine slept) skips its `calendar.ending`: the end
//! has passed, so only `calendar.ended` is left to say.

use std::collections::BTreeSet;

use chrono::{DateTime, Utc};

use super::Hook;
use crate::config::typed::Dur;

pub const ENDING: &str = "calendar.ending";
pub const ENDED: &str = "calendar.ended";

/// What one recording's timer has said so far.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct State {
    /// The `calendar.ending` hooks (labels) already due.
    pub ending: BTreeSet<String>,
    /// A tick saw the end still ahead.
    pub before: bool,
    /// `calendar.ended` was said.
    pub ended: bool,
}

/// What a tick fires.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Due {
    /// `calendar.ending` for these hooks (labels, in config order), `remaining_ms` before the end.
    Ending {
        hooks: Vec<String>,
        remaining_ms: u64,
    },
    /// `calendar.ended`, `overrun_ms` past the end.
    Ended { overrun_ms: u64 },
}

/// A `calendar.ending` hook's lead: its `match.before_end`, else `default`.
pub fn before_end(hook: &Hook, default: Dur) -> Dur {
    hook.filter.before_end.unwrap_or(default)
}

/// One look at the clock: `hooks` are every `[[hooks]]` entry (label, hook), `end` the matched
/// event's scheduled end, `now` the clock, `default` `app.hook_before_end`. Returns what fires on
/// this tick (at most one of each) and remembers it in `st`.
pub fn tick(
    st: &mut State,
    hooks: &[(String, Hook)],
    end: DateTime<Utc>,
    now: DateTime<Utc>,
    default: Dur,
) -> Vec<Due> {
    let mut out = Vec::new();
    if now < end {
        st.before = true;
        let remaining_ms = u64::try_from((end - now).num_milliseconds()).unwrap_or_default();
        let due: Vec<String> = hooks
            .iter()
            .filter(|(label, h)| {
                h.event == ENDING
                    && before_end(h, default).0 >= remaining_ms
                    && !st.ending.contains(label)
            })
            .map(|(label, _)| label.clone())
            .collect();
        if !due.is_empty() {
            st.ending.extend(due.iter().cloned());
            out.push(Due::Ending {
                hooks: due,
                remaining_ms,
            });
        }
    } else if st.before && !st.ended {
        st.ended = true;
        out.push(Due::Ended {
            overrun_ms: u64::try_from((now - end).num_milliseconds()).unwrap_or_default(),
        });
    }
    out
}

/// A span for people (`data.remaining`, `data.overrun`, reasons): hours, minutes and seconds
/// (`4m59s`, `1h0m5s`), or milliseconds under a second (`250ms`). Whole seconds, rounded down.
pub fn span(ms: u64) -> String {
    let secs = ms / 1000;
    if secs == 0 {
        return format!("{ms}ms");
    }
    let (h, m, s) = (secs / 3600, secs / 60 % 60, secs % 60);
    match (h, m) {
        (0, 0) => format!("{s}s"),
        (0, _) => format!("{m}m{s}s"),
        _ => format!("{h}h{m}m{s}s"),
    }
}

/// Whether any `[[hooks]]` entry listens for either event (the engine reads the session's
/// calendar end only then).
pub fn wanted(hooks: &[Hook]) -> bool {
    hooks.iter().any(|h| h.event == ENDING || h.event == ENDED)
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Duration;
    use serde_json::json;

    fn hook(name: &str, event: &str, before_end: Option<&str>) -> (String, Hook) {
        let mut v = json!({"name": name, "event": event, "command": ["true"]});
        if let Some(b) = before_end {
            v["match"] = json!({"before_end": b});
        }
        (name.into(), serde_json::from_value(v).unwrap())
    }

    fn at(s: i64) -> DateTime<Utc> {
        DateTime::parse_from_rfc3339("2026-11-13T15:15:00Z")
            .unwrap()
            .with_timezone(&Utc)
            + Duration::seconds(s)
    }

    const DEFAULT: Dur = Dur(300_000);

    #[test]
    fn each_hook_comes_due_at_its_own_lead_once_and_ended_once() {
        let hooks = vec![
            hook("five", ENDING, None),
            hook("one", ENDING, Some("1m")),
            hook("after", ENDED, None),
            hook("other", "record.stop", Some("10m")),
        ];
        let end = at(0);
        let mut st = State::default();
        // Ten minutes out: nothing yet.
        assert!(tick(&mut st, &hooks, end, at(-600), DEFAULT).is_empty());
        // Five minutes out: the default lead.
        assert_eq!(
            tick(&mut st, &hooks, end, at(-300), DEFAULT),
            [Due::Ending {
                hooks: vec!["five".into()],
                remaining_ms: 300_000
            }]
        );
        // Again inside its window: never twice.
        assert!(tick(&mut st, &hooks, end, at(-200), DEFAULT).is_empty());
        assert_eq!(
            tick(&mut st, &hooks, end, at(-59), DEFAULT),
            [Due::Ending {
                hooks: vec!["one".into()],
                remaining_ms: 59_000
            }]
        );
        assert!(tick(&mut st, &hooks, end, at(-1), DEFAULT).is_empty());
        // The end passes while recording: ended, with the overrun of that tick.
        assert_eq!(
            tick(
                &mut st,
                &hooks,
                end,
                at(0) + Duration::milliseconds(250),
                DEFAULT
            ),
            [Due::Ended { overrun_ms: 250 }]
        );
        // The recording keeps going: nothing more.
        assert!(tick(&mut st, &hooks, end, at(600), DEFAULT).is_empty());
        assert!(tick(&mut st, &hooks, end, at(6000), DEFAULT).is_empty());
    }

    #[test]
    fn a_start_inside_the_window_is_due_at_once_with_what_is_left() {
        let hooks = vec![hook("five", ENDING, None), hook("one", ENDING, Some("1m"))];
        let mut st = State::default();
        assert_eq!(
            tick(&mut st, &hooks, at(0), at(-30), DEFAULT),
            [Due::Ending {
                hooks: vec!["five".into(), "one".into()],
                remaining_ms: 30_000
            }]
        );
    }

    #[test]
    fn a_tick_that_jumps_the_window_says_only_ended() {
        let hooks = vec![hook("five", ENDING, None)];
        let mut st = State::default();
        assert!(tick(&mut st, &hooks, at(0), at(-900), DEFAULT).is_empty());
        // The machine slept through the window and past the end.
        assert_eq!(
            tick(&mut st, &hooks, at(0), at(120), DEFAULT),
            [Due::Ended {
                overrun_ms: 120_000
            }]
        );
        assert!(tick(&mut st, &hooks, at(0), at(130), DEFAULT).is_empty());
    }

    #[test]
    fn an_end_that_passed_before_the_recording_says_nothing() {
        let hooks = vec![hook("five", ENDING, None), hook("after", ENDED, None)];
        let mut st = State::default();
        for s in [1, 60, 600] {
            assert!(tick(&mut st, &hooks, at(0), at(s), DEFAULT).is_empty());
        }
    }

    #[test]
    fn spans_read_as_people_say_them() {
        assert_eq!(span(250), "250ms");
        assert_eq!(span(299_500), "4m59s");
        assert_eq!(span(59_000), "59s");
        assert_eq!(span(3_605_000), "1h0m5s");
    }

    #[test]
    fn the_default_lead_is_the_key_and_a_hook_may_set_its_own() {
        let (_, h) = hook("x", ENDING, Some("2m"));
        assert_eq!(before_end(&h, DEFAULT), Dur(120_000));
        let (_, h) = hook("x", ENDING, None);
        assert_eq!(before_end(&h, DEFAULT), DEFAULT);
        assert!(wanted(&[h]));
        let (_, h) = hook("x", "record.stop", None);
        assert!(!wanted(&[h]));
    }
}

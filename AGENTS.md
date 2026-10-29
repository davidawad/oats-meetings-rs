# AGENTS.md

Notes for coding agents working in this repo.

## Numbers and durations: the kinds.rs rules

`crates/core/tests/kinds.rs` scans production code (everything before a file's
`#[cfg(test)]`) in `crates/{core,ctl,ui,audio}/src` and `src`. Two of its tests fail the
quality gate on a stray literal:

- `no_timing_literals_outside_the_key_table`: no `Duration::from_secs(` / `from_millis(` /
  `from_mins(` / `from_micros(` / `from_nanos(` with a numeric literal argument anywhere but
  `crates/core/src/config/keys.rs`. Converting a value read from config is fine.
- `no_unlisted_numeric_constants_in_production_code`: no `const` / `static` of a numeric type
  (`u8`..`u128`, `usize`, `i8`..`isize`, `f32`, `f64`) unless it is on the `TRUE_CONSTANTS`
  allowlist, and no `.take(<number>)`. An allowlist entry whose constant is gone also fails.

So:

1. **Every tunable number or duration is a config key.** Add a `Key` to `KEYS` in
   `crates/core/src/config/keys.rs` with a `Kind::Int { min, max }` or
   `Kind::Duration { min_ms, max_ms }` range, a default (`Def::I` / `Def::D`, using the
   `MS`/`SEC`/`MIN`/`HOUR` units), a `doc` string, `ui` and `reload`. Add the matching field to
   the section struct in `crates/core/src/config/typed.rs` and read it from the config snapshot
   at the use site (e.g. `config.ui.drive_wait.as_duration()`). Then regenerate the projections:
   `cargo run -q --no-default-features -- config generate --dir config` (commit the changed
   `config/schema.json`, `defaults.toml`, `defaults.json`). Commit f0c1506 (`ui.drive_wait`) is a
   worked example.
2. **True constants are facts, not choices**: an FFI selector, a protocol or wire id, a file
   format version, an audio format, a colour. Either import them from a `-sys` crate (or the
   bindings crate that defines them), or add a `(path, NAME, reason)` entry to
   `TRUE_CONSTANTS` in `crates/core/tests/kinds.rs`. If it is a choice someone might tune, it is
   a key, not an allowlist entry.
3. **Run the test before finishing**:

   ```sh
   cargo test -p oats-core --test kinds
   ```

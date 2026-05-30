# Project Retro: sample

- Generated: 2026-05-26T07:00:00Z
- Mode: git+heuristic
- Window: last 5 days (2026-05-21 to 2026-05-26)
- Repo: `/tmp/sample`

## Summary

- Commits: 12
- Changed lines: 320 (+240 / -80)
- Active days: 2
- Test-related commits: 3

## Churn By Class

- source: 120 changed lines across 1 file(s), 4 commit(s)

## Themes

- Refactor: extracted helper

## Attention Items

- No tests added for the new helper

## Hotspots

- `src/main.rs` [source]: 4 commit(s), 120 changed lines

## Validation Signals

- cargo test -p sample passes

## HEURISTIC_SYSTEM

- State: stable
- Active inbox entries: 2
- Error inbox movement: no movement in window
- Operation records changed: 1
- Aging: no entries over 30 days

## Follow-Up Questions

- Should we add an integration test?

## Warnings

- test-fixture missing
- config file outdated


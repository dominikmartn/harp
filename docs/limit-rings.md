# Limit rings file format

The sidebar footer can show one double ring per AI provider: the outer ring is the share of the
weekly limit used, the inner ring the share of the 5 hour window. The app never fetches these
numbers itself. Some other tool (a menu bar app, a cron job, a script) writes them to a local JSON
file, and the sidebar re-reads that file about every 30 seconds when it changes.

The integration is off by default. Turn it on in `~/.harp/settings.toml`:

```toml
[integrations.limit_rings]
enabled = true
file = "~/.cache/limit-rings.json"
```

## Format

The file is one JSON object. A top-level key is one provider when its value is an object with at
least one of `_ts`, `used5h` or `usedWeek`; the footer shows providers sorted by key. Keys that
start with `_` and any other values are ignored, so metadata such as `"_version": 1` or
`"_meta": {...}` can sit next to the providers. A provider that is not in the file has no ring.

```json
{
  "claude": {
    "name": "Claude",
    "ok": true,
    "_ts": 1760000000,
    "used5h": 42.5,
    "reset5h": 1760010000,
    "usedWeek": 18,
    "resetWeek": 1760400000,
    "tier": "max"
  },
  "codex": {
    "ok": true,
    "_ts": 1760000000,
    "used5h": 90,
    "reset5h": 1760005000
  }
}
```

| Field | Type | Meaning |
|---|---|---|
| `name` | string, optional | Label shown under the ring. Defaults to the key. |
| `_ts` | number, required | When the numbers were read, in epoch seconds. Readings older than 15 minutes show as stale (dashed rings, no numbers). |
| `ok` | bool, optional | `false` marks the reading as failed; it shows as stale. |
| `stale` | bool, optional | `true` marks the reading as out of date; it shows as stale. |
| `used5h` | number, optional | Percent of the 5 hour window used, 0 to 100. |
| `reset5h` | number, optional | When the 5 hour window resets, in epoch seconds. A time in the past hides `used5h`. |
| `usedWeek` | number, optional | Percent of the weekly limit used, 0 to 100. |
| `resetWeek` | number, optional | When the weekly limit resets, in epoch seconds. A time in the past hides `usedWeek`. |
| `tier` | string, optional | Plan name. Parsed, not shown yet. |

A window with a missing number or reset time is drawn as a dashed track. From 80 % on the ring is
drawn in ink and its label turns semibold; at 100 % a dot fills the center.

The parser lives in `app/src/workspace/view/vertical_tabs.rs` (`mod limit_rings`), with tests next
to it.

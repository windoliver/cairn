# Policy gates

Cairn populates `policy_trace` on every mutating verb response and on read
verbs when `--explain` is set (brief §8.0.b, §14, §5.1, §5.2). Each entry
names a **gate**, a **result** (`pass`, `deny`, `error`), and an optional
short metadata `detail`. Gate names are stable; the closed producer-side
vocabulary is enumerated below.

## Negotiation

Servers advertise `cairn.mcp.v1.policy_trace` on `status.capabilities`
when they emit traces with this vocabulary. Vocabulary breaks (renames,
semantic shifts) travel with the MCP contract version
(`cairn.mcp.v2.*`) — a fresh closed `Capabilities` enum at that point —
rather than as a `.v2` suffix on this capability, matching the existing
sibling pattern.

## Gate vocabulary

| Gate string                  | Brief         | Fires on         | Typical `result` | Typical `detail`                                |
|------------------------------|---------------|------------------|------------------|--------------------------------------------------|
| `presidio_redaction`         | §5.2, §14     | every write      | `pass`           | `redacted:<tag>=<count>,…` (or absent)          |
| `prompt_injection_fence`     | §5.2          | every write      | `pass`           | (always absent — fencing wraps, never rejects)  |
| `filter_should_memorize`     | §5.2          | every write      | `pass` / `deny`  | `discard:<reason>` on deny                       |
| `visibility_floor`           | §6.3          | every write      | `pass`           | `floor:<tier>`                                   |
| `scope_check`                | §4.2          | every verb       | `pass` / `deny`  | `scope_required:<tier>` on deny                  |
| `forget_capability`          | §8            | `forget`         | `pass` / `deny`  | absent / capability code                         |
| `consent_journal_append`     | §14, §5.6     | every mutation   | `pass` / `error` | `error:<code>` on error                          |
| `read_filter_relevance`      | §5.1          | `search` / `retrieve` `--explain` | `pass`  | (per-record entries in `excluded`)               |
| `read_filter_staleness`      | §5.1          | `search` / `retrieve` `--explain` | `pass`  | (per-record entries in `excluded`)               |
| `read_filter_dedup`          | §5.1          | `search` / `retrieve` `--explain` | `pass`  | (per-record entries in `excluded`)               |

## `detail` shape

`detail` is **always body-free**. Variants in producer code:

- `none` — empty / absent on the wire.
- `discard:<reason>` — one of `volatile | tool_lookup | competing_source | low_salience | pii_blocked | injection_blocked | policy_blocked | duplicate`.
- `redacted:<tag>=<count>[,<tag>=<count>…]` — sorted by tag name.
- `floor:<tier>` — one of `private | session | project | team | org | public`.
- `scope_required:<tier>` — same enum.
- `error:<code>` — short stable static code (e.g. `wal_failure`).

Raw bytes, source content, record bodies, request URLs, and free-form
messages never appear in `detail`.

## Visibility rule

A trace entry mentioning a record (only the `excluded` field on
`--explain` does so) is only present for records the caller already had
visibility to. Tier-1-invisible records are filtered before the
rank-and-filter step that builds exclusions; their existence is never
leaked through `policy_trace`.

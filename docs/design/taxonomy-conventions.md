# Taxonomy Conventions for Profile, Insight, and Fact Records

Issue: <https://github.com/windoliver/cairn/issues/292>

Brief references: §6.1 `MemoryKind`, §6.2 `MemoryClass`, §6.3
`MemoryVisibility`, §8.0.h `lint`.

Cairn keeps `MemoryKind`, `MemoryClass`, and `MemoryVisibility` closed. User
profiles, insights, and facts are therefore conventions over the existing
taxonomy, not new enum variants.

## Canonical Mappings

| Record shape | Kind | Class | Visibility | Notes |
|---|---|---|---|---|
| Profile | `user` | `semantic` | `private` | At most one well-known profile record per actor in a vault. |
| Insight | `belief` | `semantic` | `private` by default | May promote to `project` or `team` through the normal tier promotion path. |
| Fact | `fact` | `semantic` | Per scope | Confidence is expressed through the existing evidence/confidence fields. |

## Profile Records

Profile records describe durable user or agent traits that should be available
as a composed profile. They use:

- `MemoryKind::User`
- `MemoryClass::Semantic`
- `MemoryVisibility::Private`

The well-known profile identifier is `profile:{actor_id}`. Current durable
`RecordId` and `TargetId` values remain ULIDs; adapters that need to preserve
the well-known identifier should store it in frontmatter as:

```yaml
well_known_id: profile:hmn:alice
```

There must be at most one well-known profile record for a given actor in a
vault. The body is free-form markdown. Recommended sections are:

- `Role`
- `Preferences`
- `Goals`
- `Constraints`

The section layout is a convention only and is not schema-enforced.

## Shortcut Mappings

Skill packs and bridges that expose convenience slash commands should lower
them to the closed taxonomy as follows:

| Shortcut | Kind | Class | Visibility | Required convention |
|---|---|---|---|---|
| `/remember --as-profile` | `user` | `semantic` | `private` | Set `well_known_id: profile:{actor_id}`. |
| `/remember --as-insight` | `belief` | `semantic` | `private` | Preserve `provenance.source_ids` or the summarizing operation record. |

These shortcuts are aliases over existing record shapes. They must not create
new `MemoryKind`, `MemoryClass`, or `MemoryVisibility` variants.

## Insight Records

An insight is an evidenced belief. It uses:

- `MemoryKind::Belief`
- `MemoryClass::Semantic`
- `MemoryVisibility::Private` by default

If an insight is promoted to `project` or `team`, the promotion must use the
standard consent-gated tier path from §6.3. Reasoning chains that explain how
the insight was produced should be captured separately as `reasoning` records.

An insight must keep provenance. Its provenance chain must include either the
`summarize` operation that produced it or the source records it was extracted
from. In the P0 record shape, lint treats empty `provenance.source_ids` on a
`belief` record as an orphan insight.

## Fact Records

Facts use:

- `MemoryKind::Fact`
- `MemoryClass::Semantic`
- `MemoryVisibility` based on the source scope

Confidence is carried by the existing `confidence` scalar and evidence vector.
External extractors that classify facts as `EXTRACTED`, `INFERRED`, or
`AMBIGUOUS` should map that label into their evidence metadata without adding
new taxonomy variants.

## Ambiguous Cases

| If a memory could be... | Prefer | Why |
|---|---|---|
| Profile and fact | Profile (`user` / `semantic` / `private`) | Actor-specific profile facts are consumed by profile assembly. |
| Insight and fact | Fact when directly cited; insight when inferred | `fact` is for citation-grade claims; `belief` is for evidenced inference. |
| Insight and reasoning | Insight for the conclusion; reasoning for the chain | Retrieval can ask for the belief without replaying the derivation. |
| User preference and rule | Rule when it gates behavior; profile when it describes preference | Rules constrain actions; profile records inform personalization. |
| Reference and fact | Reference for the source artifact; fact for extracted claim | Source identity and extracted claim have different lifecycle rules. |

## Lint Rules

`cairn lint` emits warnings, not errors, for taxonomy-convention drift:

- `orphan_insight` — a `belief` record has empty `provenance.source_ids`.
- `misclassified_profile` — multiple records carry the same
  `well_known_id: profile:{actor_id}`.
- `wrong_class_for_kind` — a record's class does not match the canonical class
  table below.

## Canonical Class Table

| Kind | Canonical class |
|---|---|
| `user` | `semantic` |
| `feedback` | `episodic` |
| `project` | `semantic` |
| `reference` | `semantic` |
| `fact` | `semantic` |
| `belief` | `semantic` |
| `opinion` | `semantic` |
| `event` | `episodic` |
| `entity` | `semantic` |
| `workflow` | `procedural` |
| `rule` | `procedural` |
| `strategy_success` | `procedural` |
| `strategy_failure` | `procedural` |
| `trace` | `episodic` |
| `reasoning` | `episodic` |
| `playbook` | `procedural` |
| `sensor_observation` | `episodic` |
| `user_signal` | `episodic` |
| `knowledge_gap` | `semantic` |

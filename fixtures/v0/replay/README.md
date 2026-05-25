# Replay Cassettes

Replay cassettes are deterministic JSON manifests for CI and release gates. They seed a temporary SQLite vault, run local replay actions, and compare each action against golden expectations without external network or LLM calls.

## Extended Domain Suites

`research_domain` covers long-horizon memory across literature review and experiment-planning turns. It checks multi-session coherence by preserving both project context and later synthesis, search relevance for the hypothesis thread, summary retrieval, and privacy/forget handling for a temporary embargo note.

`engineering_domain` covers implementation work across investigation and patch sessions. It checks long-horizon memory for a concurrency fix, multi-session coherence between diagnosis and verification, search relevance for the locking decision, summary retrieval, and privacy/forget handling for a throwaway credential.

`support_domain` covers customer support workflows across intake and escalation sessions. It checks long-horizon memory for customer context, multi-session coherence between ticket intake and resolution, search relevance for the remediation plan, summary retrieval, and privacy/forget handling for a sensitive contact note.

## Golden expectations

Each domain suite includes golden expectations for:

- long-horizon memory: `retrieve_session`, `retrieve_turn`, and `summarize` actions must recover the expected trace and summary records.
- multi-session coherence: records from related sessions use stable domain terms that are retrieved by deterministic keyword search.
- privacy/forget: `forget_record` tombstones the sensitive fixture record and verifies a follow-up search no longer returns it.
- search relevance: keyword queries assert exact top-hit record IDs, keeping replay safe for CI without semantic embedding or network dependencies.

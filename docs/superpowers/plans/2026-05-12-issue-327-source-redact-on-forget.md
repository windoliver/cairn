# Issue 327 Source Redact On Forget Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Wire record-level `forget` end-to-end so it can emit `source_forget` journal rows, honor `source.redact_on_forget`, and rewrite matching source files to metadata-only stubs.

**Architecture:** Reuse the existing record store and consent journal rather than inventing a separate forget pipeline. The CLI `forget --record` path will load the active record, tombstone it, append a body-free `source_forget` event keyed by `provenance.source_hash`, and optionally scan `sources/` for files whose SHA-256 matches that hash so they can be rewritten to a metadata stub.

**Tech Stack:** Rust, `rusqlite`, existing `SqliteMemoryStore`, CLI envelope helpers, generated docs.

---

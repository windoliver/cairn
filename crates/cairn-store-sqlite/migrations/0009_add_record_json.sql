-- Migration 0009: add record_json column to records for full MemoryRecord
-- round-trip. The individual columns (body, provenance, etc.) are retained for
-- SQL filtering; record_json is the canonical round-trip form for reads.
ALTER TABLE records ADD COLUMN record_json TEXT;

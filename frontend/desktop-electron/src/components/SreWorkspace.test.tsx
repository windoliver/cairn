import "@testing-library/jest-dom/vitest";
import { render, screen } from "@testing-library/react";
import { describe, expect, it } from "vitest";
import type { DesktopSreReport } from "../api/types";
import { SreWorkspace } from "./SreWorkspace";

const report: DesktopSreReport = {
  schema_version: 1,
  captured_at_ms: 1700000000000,
  vault: { id_hash: "sha256:vault", name: "Fixture" },
  workflow: {
    status: "warning",
    oldest_queued_age_ms: 742000,
    longest_held_lease_ms: 91000,
    dead_letter_count: 1,
    kinds: [
      {
        kind: "expire.tier",
        queued: 2,
        leased: 1,
        done_recent: 3,
        failed_recent: 0,
        oldest_queued_age_ms: 742000,
        last_success_age_ms: 50000,
        backlog_threshold_ms: 600000,
        status: "warning",
      },
    ],
  },
  rehydration: {
    status: "ok",
    latest_latency_ms: 2100,
    p95_latency_ms: 2210,
    slo_ms: 3000,
    sample_count: 12,
    last_gate: null,
  },
  projection: {
    status: "warning",
    nexus_state: "degraded",
    nexus_reason: "sidecar_unavailable",
    targets: [
      {
        target: "bm25s_lexical",
        current: 11,
        stale: 1,
        failed: 2,
        missing: 3,
        max_lag_ms: 4500,
        last_rebuild_latency_ms: 88,
        status: "warning",
      },
    ],
  },
  search: {
    status: "warning",
    modes: [
      {
        mode: "semantic",
        advertised: true,
        invocations: 42,
        degraded: 3,
        failed: 0,
        p95_latency_ms: 54,
        status: "warning",
      },
    ],
  },
  gates: {
    status: "fail",
    gates: [
      {
        name: "migration_backlog",
        status: "fail",
        measured: 742000,
        threshold: 600000,
        unit: "ms",
        detail: "SECRET_PRIVATE_TOKEN private body query text",
      },
    ],
  },
  privacy: { scrubbed: true, forbidden_field_count: 0 },
};

describe("SreWorkspace", () => {
  it("renders SRE sections without private payload text", () => {
    render(<SreWorkspace report={report} />);

    expect(screen.getByText("Workflow")).toBeInTheDocument();
    expect(screen.getByText("Rehydration")).toBeInTheDocument();
    expect(screen.getByText("Projection")).toBeInTheDocument();
    expect(screen.getByText("Search")).toBeInTheDocument();
    expect(screen.getByText("Release Gates")).toBeInTheDocument();
    expect(screen.getByText("expire.tier")).toBeInTheDocument();
    expect(screen.getByText("91s")).toBeInTheDocument();
    expect(screen.getByText("50s")).toBeInTheDocument();
    expect(screen.getByText("2 failed")).toBeInTheDocument();
    expect(screen.getByText("54ms p95")).toBeInTheDocument();
    expect(screen.getByText("threshold 600000ms")).toBeInTheDocument();
    expect(screen.queryByText(/SECRET_PRIVATE_TOKEN/)).not.toBeInTheDocument();
  });

  it("shows loading state when report is absent", () => {
    render(<SreWorkspace report={null} />);

    expect(screen.getByText("SRE report loading")).toBeInTheDocument();
  });

  it("shows unavailable state when SRE loading fails", () => {
    render(<SreWorkspace report={null} error="SRE endpoint unavailable" />);

    expect(screen.getByText("SRE report unavailable")).toBeInTheDocument();
    expect(screen.getByText("The SRE endpoint could not be loaded.")).toBeInTheDocument();
    expect(screen.queryByText("SRE endpoint unavailable")).not.toBeInTheDocument();
  });
});

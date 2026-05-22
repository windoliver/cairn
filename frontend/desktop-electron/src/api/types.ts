export type DesktopVaultSummary = {
  id: string;
  name: string;
  root: string;
  recordCount: number;
  folderCount: number;
};

export type DesktopFolder = {
  id: string;
  name: string;
  parentId: string | null;
};

export type DesktopRecordSummary = {
  id: string;
  title: string;
  folderId: string;
  kind: string;
  tags: string[];
  version: number;
  confidence: number;
};

export type DesktopRecordDetail = DesktopRecordSummary & {
  body: string;
  backendHash: string;
  sourceHash: string;
  links: string[];
};

export type DesktopGraph = {
  nodes: Array<{ id: string; label: string; kind: string; group: string }>;
  edges: Array<{ id: string; source: string; target: string; label: string }>;
};

export type DesktopSessionTree = {
  root: string;
  nodes: Array<{
    id: string;
    parentId: string | null;
    branchKind: string | null;
    atTurnId: string | null;
    toolCallId: string | null;
    children: string[];
  }>;
  merges: Array<{
    source: string;
    destination: string;
    strategy: string;
    summaryRecordId: string | null;
    firstTurnId: string | null;
    lastTurnId: string | null;
    appliedAtTurnId: string;
  }>;
};

export type DesktopSearchResult = {
  recordId: string;
  title: string;
  snippet: string;
  score: number;
};

export type DesktopLintFinding = {
  id: string;
  severity: string;
  recordId: string | null;
  message: string;
};

export type DesktopRejectedField = {
  field: string;
  code: string;
  message: string;
};

export type DesktopReconcilePreviewRequest = {
  targetId: string;
  expectedVersion: number;
  backendHash: string;
  fieldDiff: Record<string, unknown>;
};

export type DesktopReconcilePreview = {
  accepted: boolean;
  targetId: string;
  expectedVersion: number;
  mutableDiff: Record<string, unknown>;
  rejectedFields: DesktopRejectedField[];
};

export type DesktopReconcileApplyRequest = DesktopReconcilePreviewRequest;

export type DesktopReconcileApplyResult = {
  accepted: boolean;
  record: DesktopRecordDetail | null;
  rejectedFields: DesktopRejectedField[];
};

export type SreStatus = "ok" | "warning" | "fail" | "unknown";

export type DesktopSreGateResult = {
  name: string;
  status: SreStatus;
  measured: number | null;
  threshold: number | null;
  unit: string;
  detail: string | null;
};

export type DesktopSreReport = {
  schema_version: number;
  captured_at_ms: number;
  vault: {
    id_hash: string;
    name: string;
  };
  workflow: {
    status: SreStatus;
    oldest_queued_age_ms: number | null;
    longest_held_lease_ms: number | null;
    dead_letter_count: number;
    kinds: Array<{
      kind: string;
      queued: number;
      leased: number;
      done_recent: number;
      failed_recent: number;
      oldest_queued_age_ms: number | null;
      last_success_age_ms: number | null;
      backlog_threshold_ms: number;
      status: SreStatus;
    }>;
  };
  rehydration: {
    status: SreStatus;
    latest_latency_ms: number | null;
    p95_latency_ms: number | null;
    slo_ms: number;
    sample_count: number;
    last_gate: DesktopSreGateResult | null;
  };
  projection: {
    status: SreStatus;
    nexus_state: string;
    nexus_reason: string | null;
    targets: Array<{
      target: string;
      current: number;
      stale: number;
      failed: number;
      missing: number;
      max_lag_ms: number | null;
      last_rebuild_latency_ms: number | null;
      status: SreStatus;
    }>;
  };
  search: {
    status: SreStatus;
    modes: Array<{
      mode: string;
      advertised: boolean;
      invocations: number;
      degraded: number;
      failed: number;
      p95_latency_ms: number | null;
      status: SreStatus;
    }>;
  };
  gates: {
    status: SreStatus;
    gates: DesktopSreGateResult[];
  };
  privacy: {
    scrubbed: boolean;
    forbidden_field_count: number;
  };
};

export type DesktopApiError = Error & {
  code: string;
  status: number;
};

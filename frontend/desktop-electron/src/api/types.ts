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

export type DesktopApiError = Error & {
  code: string;
  status: number;
};

import type {
  DesktopApiError,
  DesktopFolder,
  DesktopGraph,
  DesktopLintFinding,
  DesktopRecordDetail,
  DesktopRecordSummary,
  DesktopReconcileApplyRequest,
  DesktopReconcileApplyResult,
  DesktopReconcilePreview,
  DesktopReconcilePreviewRequest,
  DesktopSearchResult,
  DesktopVaultSummary,
} from "./types";

export class DesktopApiClient {
  constructor(private readonly baseUrl: string) {}

  vault(): Promise<DesktopVaultSummary> {
    return this.get("/api/v1/vault");
  }

  folders(): Promise<DesktopFolder[]> {
    return this.get("/api/v1/folders");
  }

  records(): Promise<DesktopRecordSummary[]> {
    return this.get("/api/v1/records");
  }

  record(id: string): Promise<DesktopRecordDetail> {
    return this.get(`/api/v1/records/${encodeURIComponent(id)}`);
  }

  graph(): Promise<DesktopGraph> {
    return this.get("/api/v1/graph");
  }

  search(query: string): Promise<DesktopSearchResult[]> {
    return this.get(`/api/v1/search?q=${encodeURIComponent(query)}`);
  }

  lint(): Promise<DesktopLintFinding[]> {
    return this.get("/api/v1/lint");
  }

  previewReconcile(request: DesktopReconcilePreviewRequest): Promise<DesktopReconcilePreview> {
    return this.post("/api/v1/reconcile/preview", request);
  }

  applyReconcile(request: DesktopReconcileApplyRequest): Promise<DesktopReconcileApplyResult> {
    return this.post("/api/v1/reconcile/apply", request);
  }

  private async get<T>(path: string): Promise<T> {
    const response = await fetch(`${this.baseUrl}${path}`);
    return readJson<T>(response);
  }

  private async post<T>(path: string, body: unknown): Promise<T> {
    const response = await fetch(`${this.baseUrl}${path}`, {
      method: "POST",
      headers: { "content-type": "application/json" },
      body: JSON.stringify(body),
    });
    return readJson<T>(response);
  }
}

async function readJson<T>(response: Response): Promise<T> {
  const body = await response.json();
  if (response.ok) {
    return body as T;
  }
  const error = new Error(body.message ?? "Desktop API request failed") as DesktopApiError;
  error.code = body.code ?? "desktop_api_error";
  error.status = response.status;
  throw error;
}

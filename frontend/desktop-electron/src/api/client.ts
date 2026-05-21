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
  DesktopSessionTree,
  DesktopSreReport,
  DesktopVaultSummary,
} from "./types";

export class DesktopApiClient {
  private readonly baseUrl: string;

  constructor(baseUrl: string) {
    this.baseUrl = baseUrl.replace(/\/+$/, "");
  }

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

  sessionTree(): Promise<DesktopSessionTree> {
    return this.get("/api/v1/session-tree");
  }

  search(query: string): Promise<DesktopSearchResult[]> {
    return this.get(`/api/v1/search?q=${encodeURIComponent(query)}`);
  }

  lint(): Promise<DesktopLintFinding[]> {
    return this.get("/api/v1/lint");
  }

  sre(): Promise<DesktopSreReport> {
    return this.get("/api/v1/sre");
  }

  previewReconcile(request: DesktopReconcilePreviewRequest): Promise<DesktopReconcilePreview> {
    return this.post("/api/v1/reconcile/preview", request);
  }

  applyReconcile(request: DesktopReconcileApplyRequest): Promise<DesktopReconcileApplyResult> {
    return this.post("/api/v1/reconcile/apply", request);
  }

  private async get<T>(path: string): Promise<T> {
    const response = await requestJson(`${this.baseUrl}${path}`);
    return readJson<T>(response);
  }

  private async post<T>(path: string, body: unknown): Promise<T> {
    const response = await requestJson(`${this.baseUrl}${path}`, {
      method: "POST",
      headers: { "content-type": "application/json" },
      body: JSON.stringify(body),
    });
    return readJson<T>(response);
  }
}

async function requestJson(input: RequestInfo | URL, init?: RequestInit): Promise<Response> {
  try {
    return init ? await fetch(input, init) : await fetch(input);
  } catch {
    throw desktopApiError("Desktop API request failed", "desktop_api_error", 0);
  }
}

async function readJson<T>(response: Response): Promise<T> {
  const body = await parseJsonBody(response);
  if (response.ok) {
    if (!body.ok) {
      throw desktopApiError(
        "Desktop API response was not valid JSON",
        "desktop_api_error",
        response.status,
      );
    }
    return body.value as T;
  }
  const errorBody = body.ok && isObject(body.value) ? body.value : {};
  throw desktopApiError(
    typeof errorBody.message === "string" ? errorBody.message : "Desktop API request failed",
    typeof errorBody.code === "string" ? errorBody.code : "desktop_api_error",
    response.status,
  );
}

type JsonBody =
  | {
      ok: true;
      value: unknown;
    }
  | {
      ok: false;
    };

async function parseJsonBody(response: Response): Promise<JsonBody> {
  try {
    return { ok: true, value: await response.json() };
  } catch {
    return { ok: false };
  }
}

function isObject(value: unknown): value is Record<string, unknown> {
  return typeof value === "object" && value !== null;
}

function desktopApiError(message: string, code: string, status: number): DesktopApiError {
  const error = new Error(message) as DesktopApiError;
  error.code = code;
  error.status = status;
  return error;
}

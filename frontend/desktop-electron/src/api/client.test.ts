import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { DesktopApiClient } from "./client";

describe("DesktopApiClient", () => {
  const fetchMock = vi.fn();

  beforeEach(() => {
    vi.stubGlobal("fetch", fetchMock);
  });

  afterEach(() => {
    vi.unstubAllGlobals();
    fetchMock.mockReset();
  });

  it("loads records from the backend", async () => {
    fetchMock.mockResolvedValueOnce(
      new Response(JSON.stringify([{ id: "rec-alpha-001", title: "Project memory scaffold" }]), {
        status: 200,
        headers: { "content-type": "application/json" },
      }),
    );

    const client = new DesktopApiClient("http://127.0.0.1:4000");
    const records = await client.records();

    expect(fetchMock).toHaveBeenCalledWith("http://127.0.0.1:4000/api/v1/records");
    expect(records[0].id).toBe("rec-alpha-001");
  });

  it("loads session tree data from the backend", async () => {
    fetchMock.mockResolvedValueOnce(
      new Response(JSON.stringify({ root: "session-root", nodes: [], merges: [] }), {
        status: 200,
        headers: { "content-type": "application/json" },
      }),
    );

    const client = new DesktopApiClient("http://127.0.0.1:4000");
    const tree = await client.sessionTree();

    expect(fetchMock).toHaveBeenCalledWith("http://127.0.0.1:4000/api/v1/session-tree");
    expect(tree.root).toBe("session-root");
  });

  it("normalizes trailing slashes in the backend base URL", async () => {
    fetchMock.mockResolvedValueOnce(
      new Response(JSON.stringify([{ id: "rec-alpha-001", title: "Project memory scaffold" }]), {
        status: 200,
        headers: { "content-type": "application/json" },
      }),
    );

    const client = new DesktopApiClient("http://127.0.0.1:4000/");
    await client.records();

    expect(fetchMock).toHaveBeenCalledWith("http://127.0.0.1:4000/api/v1/records");
  });

  it("throws structured errors for failed requests", async () => {
    fetchMock.mockResolvedValueOnce(
      new Response(JSON.stringify({ code: "record_not_found", message: "Record was not found" }), {
        status: 404,
        headers: { "content-type": "application/json" },
      }),
    );

    const client = new DesktopApiClient("http://127.0.0.1:4000");
    await expect(client.record("missing")).rejects.toMatchObject({
      code: "record_not_found",
    });
  });

  it("throws structured errors for non-json failed requests", async () => {
    fetchMock.mockResolvedValueOnce(
      new Response("gateway unavailable", {
        status: 502,
        headers: { "content-type": "text/plain" },
      }),
    );

    const client = new DesktopApiClient("http://127.0.0.1:4000");
    await expect(client.record("rec-alpha-001")).rejects.toMatchObject({
      code: "desktop_api_error",
      status: 502,
      message: "Desktop API request failed",
    });
  });

  it("throws structured errors for network failures", async () => {
    fetchMock.mockRejectedValueOnce(new TypeError("Failed to fetch"));

    const client = new DesktopApiClient("http://127.0.0.1:4000");
    await expect(client.record("rec-alpha-001")).rejects.toMatchObject({
      code: "desktop_api_error",
      status: 0,
      message: "Desktop API request failed",
    });
  });

  it("throws structured errors for invalid json successful requests", async () => {
    fetchMock.mockResolvedValueOnce(
      new Response("not-json", {
        status: 200,
        headers: { "content-type": "application/json" },
      }),
    );

    const client = new DesktopApiClient("http://127.0.0.1:4000");
    await expect(client.records()).rejects.toMatchObject({
      code: "desktop_api_error",
      status: 200,
      message: "Desktop API response was not valid JSON",
    });
  });

  it("applies reconcile requests through the backend", async () => {
    fetchMock.mockResolvedValueOnce(
      new Response(JSON.stringify({ accepted: true, record: { id: "rec-alpha-001" }, rejectedFields: [] }), {
        status: 200,
        headers: { "content-type": "application/json" },
      }),
    );

    const client = new DesktopApiClient("http://127.0.0.1:4000");
    const result = await client.applyReconcile({
      targetId: "rec-alpha-001",
      expectedVersion: 2,
      backendHash: "sha256:fixture-alpha-001",
      fieldDiff: { body: "Applied body" },
    });

    expect(fetchMock).toHaveBeenCalledWith("http://127.0.0.1:4000/api/v1/reconcile/apply", {
      method: "POST",
      headers: { "content-type": "application/json" },
      body: JSON.stringify({
        targetId: "rec-alpha-001",
        expectedVersion: 2,
        backendHash: "sha256:fixture-alpha-001",
        fieldDiff: { body: "Applied body" },
      }),
    });
    expect(result.accepted).toBe(true);
  });

  it("loads the SRE report", async () => {
    fetchMock.mockResolvedValueOnce(
      new Response(
        JSON.stringify({
          schema_version: 1,
          captured_at_ms: 1700000000000,
          vault: { id_hash: "sha256:vault", name: "Fixture" },
          workflow: {
            status: "warning",
            oldest_queued_age_ms: 742000,
            longest_held_lease_ms: null,
            dead_letter_count: 1,
            kinds: [],
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
            targets: [],
          },
          search: { status: "warning", modes: [] },
          gates: { status: "warning", gates: [] },
          privacy: { scrubbed: true, forbidden_field_count: 0 },
        }),
        {
          status: 200,
          headers: { "content-type": "application/json" },
        },
      ),
    );

    const client = new DesktopApiClient("http://127.0.0.1:4000");
    const report = await client.sre();

    expect(fetchMock).toHaveBeenCalledWith("http://127.0.0.1:4000/api/v1/sre");
    expect(report.workflow.status).toBe("warning");
  });

  it("attaches Authorization: Bearer when a token is configured", async () => {
    fetchMock.mockResolvedValueOnce(
      new Response("[]", {
        status: 200,
        headers: { "content-type": "application/json" },
      }),
    );
    const client = new DesktopApiClient("http://127.0.0.1:4000", "test-token-xyz");
    await client.records();

    expect(fetchMock).toHaveBeenCalledWith(
      "http://127.0.0.1:4000/api/v1/records",
      expect.objectContaining({
        headers: { authorization: "Bearer test-token-xyz" },
      }),
    );
  });

  it("attaches Authorization: Bearer on POST when a token is configured", async () => {
    fetchMock.mockResolvedValueOnce(
      new Response("{}", {
        status: 200,
        headers: { "content-type": "application/json" },
      }),
    );
    const client = new DesktopApiClient("http://127.0.0.1:4000", "test-token-xyz");
    await client.previewReconcile({} as never);

    expect(fetchMock).toHaveBeenCalledWith(
      "http://127.0.0.1:4000/api/v1/reconcile/preview",
      expect.objectContaining({
        method: "POST",
        headers: expect.objectContaining({
          authorization: "Bearer test-token-xyz",
        }),
      }),
    );
  });
});

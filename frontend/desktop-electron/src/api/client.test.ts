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
});

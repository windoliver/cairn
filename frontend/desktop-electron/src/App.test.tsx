import "@testing-library/jest-dom/vitest";
import { act, render, screen, waitFor } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { afterEach, describe, expect, it, vi } from "vitest";
import { App, resolveDesktopApiBaseUrl } from "./App";

const api = {
  vault: vi.fn().mockResolvedValue({
    id: "desktop-alpha",
    name: "Desktop Alpha Fixture",
    root: "fixtures/desktop-gui-alpha",
    recordCount: 3,
    folderCount: 2,
  }),
  folders: vi
    .fn()
    .mockResolvedValue([{ id: "folder-core", name: "Core Memories", parentId: null }]),
  records: vi.fn().mockResolvedValue([
    {
      id: "rec-alpha-001",
      title: "Project memory scaffold",
      folderId: "folder-core",
      kind: "skill",
      tags: ["alpha"],
      version: 2,
      confidence: 0.86,
    },
  ]),
  record: vi.fn().mockResolvedValue({
    id: "rec-alpha-001",
    title: "Project memory scaffold",
    folderId: "folder-core",
    body: "Markdown body",
    kind: "skill",
    tags: ["alpha"],
    version: 2,
    backendHash: "sha256:fixture-alpha-001",
    confidence: 0.86,
    sourceHash: "sha256:source-alpha-001",
    links: ["rec-alpha-002"],
  }),
  graph: vi.fn().mockResolvedValue({
    nodes: [
      {
        id: "rec-alpha-001",
        label: "Project memory scaffold",
        kind: "skill",
        group: "folder-core",
      },
    ],
    edges: [
      {
        id: "rec-alpha-001--rec-alpha-002",
        source: "rec-alpha-001",
        target: "rec-alpha-002",
        label: "wikilink",
      },
    ],
  }),
  lint: vi.fn().mockResolvedValue([
    {
      id: "lint-alpha-001",
      severity: "warning",
      recordId: "rec-alpha-001",
      message: "Source hash is stale",
    },
  ]),
  search: vi.fn().mockResolvedValue([]),
  previewReconcile: vi.fn(),
  applyReconcile: vi.fn(),
};

afterEach(() => {
  vi.clearAllMocks();
  vi.unstubAllGlobals();
  delete window.cairnDesktop;
});

describe("App", () => {
  it("prefers the Electron preload API base URL when available", () => {
    Object.defineProperty(window, "cairnDesktop", {
      configurable: true,
      value: { apiBaseUrl: "http://127.0.0.1:49152" },
    });

    expect(resolveDesktopApiBaseUrl()).toBe("http://127.0.0.1:49152");
  });

  it("renders the vault inspector and loaded record", async () => {
    render(<App api={api} />);

    await waitFor(() => expect(screen.getByText("Desktop Alpha Fixture")).toBeInTheDocument());
    expect(screen.getAllByText("Project memory scaffold").length).toBeGreaterThan(0);
    expect(screen.getByText("Markdown body")).toBeInTheDocument();
    expect(screen.getByText("Graph")).toBeInTheDocument();
    expect(screen.getByText("Lint")).toBeInTheDocument();
  });

  it("uses a stable default API client across rerenders", async () => {
    const fetchMock = vi.fn(async (input: RequestInfo | URL) => {
      const url = input.toString();
      if (url.endsWith("/api/v1/vault")) {
        return jsonResponse({
          id: "desktop-alpha",
          name: "Desktop Alpha Fixture",
          root: "fixtures/desktop-gui-alpha",
          recordCount: 1,
          folderCount: 1,
        });
      }
      if (url.endsWith("/api/v1/folders")) {
        return jsonResponse([{ id: "folder-core", name: "Core Memories", parentId: null }]);
      }
      if (url.endsWith("/api/v1/records")) {
        return jsonResponse([
          {
            id: "rec-alpha-001",
            title: "Project memory scaffold",
            folderId: "folder-core",
            kind: "skill",
            tags: ["alpha"],
            version: 2,
            confidence: 0.86,
          },
        ]);
      }
      if (url.endsWith("/api/v1/records/rec-alpha-001")) {
        return jsonResponse({
          id: "rec-alpha-001",
          title: "Project memory scaffold",
          folderId: "folder-core",
          body: "Markdown body",
          kind: "skill",
          tags: ["alpha"],
          version: 2,
          backendHash: "sha256:fixture-alpha-001",
          confidence: 0.86,
          sourceHash: "sha256:source-alpha-001",
          links: ["rec-alpha-002"],
        });
      }
      if (url.endsWith("/api/v1/graph")) {
        return jsonResponse({ nodes: [], edges: [] });
      }
      if (url.endsWith("/api/v1/lint")) {
        return jsonResponse([]);
      }
      throw new Error(`Unexpected fetch ${url}`);
    });
    vi.stubGlobal("fetch", fetchMock);

    render(<App />);

    await screen.findByText("Desktop Alpha Fixture");
    await new Promise((resolve) => setTimeout(resolve, 50));

    expect(fetchMock).toHaveBeenCalledTimes(6);
  });

  it("shows record selection failures inline", async () => {
    const user = userEvent.setup();
    api.records.mockResolvedValueOnce([
      {
        id: "rec-alpha-001",
        title: "Project memory scaffold",
        folderId: "folder-core",
        kind: "skill",
        tags: ["alpha"],
        version: 2,
        confidence: 0.86,
      },
      {
        id: "rec-alpha-002",
        title: "Second memory",
        folderId: "folder-core",
        kind: "profile",
        tags: ["alpha"],
        version: 1,
        confidence: 0.72,
      },
    ]);
    api.record
      .mockResolvedValueOnce({
        id: "rec-alpha-001",
        title: "Project memory scaffold",
        folderId: "folder-core",
        body: "Markdown body",
        kind: "skill",
        tags: ["alpha"],
        version: 2,
        backendHash: "sha256:fixture-alpha-001",
        confidence: 0.86,
        sourceHash: "sha256:source-alpha-001",
        links: ["rec-alpha-002"],
      })
      .mockRejectedValueOnce(new Error("Record detail unavailable"));

    render(<App api={api} />);

    await screen.findAllByText("Project memory scaffold");
    await user.click(screen.getByRole("button", { name: /Second memory/ }));

    expect(await screen.findByText("Record detail unavailable")).toBeInTheDocument();
  });

  it("keeps the newest selected record when record detail responses arrive out of order", async () => {
    const user = userEvent.setup();
    let resolveSecond: (record: Awaited<ReturnType<typeof api.record>>) => void;
    const secondRecord = new Promise<Awaited<ReturnType<typeof api.record>>>((resolve) => {
      resolveSecond = resolve;
    });
    api.records.mockResolvedValueOnce([
      {
        id: "rec-alpha-001",
        title: "Project memory scaffold",
        folderId: "folder-core",
        kind: "skill",
        tags: ["alpha"],
        version: 2,
        confidence: 0.86,
      },
      {
        id: "rec-alpha-002",
        title: "Second memory",
        folderId: "folder-core",
        kind: "profile",
        tags: ["alpha"],
        version: 1,
        confidence: 0.72,
      },
    ]);
    api.record
      .mockResolvedValueOnce({
        id: "rec-alpha-001",
        title: "Project memory scaffold",
        folderId: "folder-core",
        body: "Markdown body",
        kind: "skill",
        tags: ["alpha"],
        version: 2,
        backendHash: "sha256:fixture-alpha-001",
        confidence: 0.86,
        sourceHash: "sha256:source-alpha-001",
        links: ["rec-alpha-002"],
      })
      .mockReturnValueOnce(secondRecord)
      .mockResolvedValueOnce({
        id: "rec-alpha-001",
        title: "Project memory scaffold",
        folderId: "folder-core",
        body: "Fresh Markdown body",
        kind: "skill",
        tags: ["alpha"],
        version: 2,
        backendHash: "sha256:fixture-alpha-001",
        confidence: 0.86,
        sourceHash: "sha256:source-alpha-001",
        links: ["rec-alpha-002"],
      });

    render(<App api={api} />);

    await screen.findByDisplayValue("Markdown body");
    await user.click(screen.getByRole("button", { name: /Second memory/ }));
    await user.click(screen.getByRole("button", { name: /Project memory scaffold/ }));
    expect(await screen.findByDisplayValue("Fresh Markdown body")).toBeInTheDocument();

    await act(async () => {
      resolveSecond!({
        id: "rec-alpha-002",
        title: "Second memory",
        folderId: "folder-core",
        body: "Stale second body",
        kind: "profile",
        tags: ["alpha"],
        version: 1,
        backendHash: "sha256:fixture-alpha-002",
        confidence: 0.72,
        sourceHash: "sha256:source-alpha-002",
        links: ["rec-alpha-001"],
      });
    });

    expect(screen.queryByDisplayValue("Stale second body")).not.toBeInTheDocument();
    expect(screen.getByDisplayValue("Fresh Markdown body")).toBeInTheDocument();
  });

  it("reviews a reconcile edit through the backend client", async () => {
    const user = userEvent.setup();
    api.previewReconcile.mockResolvedValueOnce({
      accepted: true,
      targetId: "rec-alpha-001",
      expectedVersion: 2,
      mutableDiff: { body: "Markdown body" },
      rejectedFields: [],
    });

    render(<App api={api} />);

    await screen.findAllByText("Project memory scaffold");
    await user.click(screen.getByRole("button", { name: "Review reconcile" }));

    expect(await screen.findByText("Ready to apply")).toBeInTheDocument();
    expect(api.previewReconcile).toHaveBeenCalledWith({
      targetId: "rec-alpha-001",
      expectedVersion: 2,
      backendHash: "sha256:fixture-alpha-001",
      fieldDiff: { body: "Markdown body" },
    });
  });

  it("preserves an empty body draft in reconcile requests", async () => {
    const user = userEvent.setup();
    api.previewReconcile.mockResolvedValueOnce({
      accepted: true,
      targetId: "rec-alpha-001",
      expectedVersion: 2,
      mutableDiff: { body: "" },
      rejectedFields: [],
    });

    render(<App api={api} />);

    const body = await screen.findByLabelText("Record body");
    await user.clear(body);
    await user.click(screen.getByRole("button", { name: "Review reconcile" }));

    expect(api.previewReconcile).toHaveBeenCalledWith({
      targetId: "rec-alpha-001",
      expectedVersion: 2,
      backendHash: "sha256:fixture-alpha-001",
      fieldDiff: { body: "" },
    });
  });

  it("clears reconcile readiness when the reviewed draft changes", async () => {
    const user = userEvent.setup();
    api.previewReconcile.mockResolvedValueOnce({
      accepted: true,
      targetId: "rec-alpha-001",
      expectedVersion: 2,
      mutableDiff: { body: "Markdown body" },
      rejectedFields: [],
    });

    render(<App api={api} />);

    const body = await screen.findByLabelText("Record body");
    await user.click(screen.getByRole("button", { name: "Review reconcile" }));
    expect(await screen.findByText("Ready to apply")).toBeInTheDocument();

    await user.type(body, " changed");

    expect(screen.queryByRole("button", { name: "Apply reconcile" })).not.toBeInTheDocument();
    expect(screen.queryByText("Ready to apply")).not.toBeInTheDocument();
  });

  it("shows reconcile review request failures inline", async () => {
    const user = userEvent.setup();
    api.previewReconcile.mockRejectedValueOnce(new Error("Preview service unavailable"));

    render(<App api={api} />);

    await screen.findAllByText("Project memory scaffold");
    await user.click(screen.getByRole("button", { name: "Review reconcile" }));

    expect(await screen.findByText("Preview service unavailable")).toBeInTheDocument();
    expect(screen.queryByRole("button", { name: "Apply reconcile" })).not.toBeInTheDocument();
  });

  it("shows reconcile apply request failures inline", async () => {
    const user = userEvent.setup();
    api.previewReconcile.mockResolvedValueOnce({
      accepted: true,
      targetId: "rec-alpha-001",
      expectedVersion: 2,
      mutableDiff: { body: "Markdown body" },
      rejectedFields: [],
    });
    api.applyReconcile.mockRejectedValueOnce(new Error("Apply service unavailable"));

    render(<App api={api} />);

    await screen.findAllByText("Project memory scaffold");
    await user.click(screen.getByRole("button", { name: "Review reconcile" }));
    await user.click(await screen.findByRole("button", { name: "Apply reconcile" }));

    expect(await screen.findByText("Apply service unavailable")).toBeInTheDocument();
    expect(screen.queryByText("Applied")).not.toBeInTheDocument();
  });

  it("applies a reviewed reconcile edit through the backend client", async () => {
    const user = userEvent.setup();
    api.previewReconcile.mockResolvedValueOnce({
      accepted: true,
      targetId: "rec-alpha-001",
      expectedVersion: 2,
      mutableDiff: { body: "Markdown body" },
      rejectedFields: [],
    });
    api.applyReconcile.mockResolvedValueOnce({
      accepted: true,
      record: {
        id: "rec-alpha-001",
        title: "Project memory scaffold",
        folderId: "folder-core",
        body: "Applied Markdown body",
        kind: "skill",
        tags: ["alpha"],
        version: 3,
        backendHash: "sha256:fixture-alpha-001-next",
        confidence: 0.86,
        sourceHash: "sha256:source-alpha-001",
        links: ["rec-alpha-002"],
      },
      rejectedFields: [],
    });

    render(<App api={api} />);

    await screen.findAllByText("Project memory scaffold");
    await user.click(screen.getByRole("button", { name: "Review reconcile" }));
    await user.click(await screen.findByRole("button", { name: "Apply reconcile" }));

    expect(await screen.findByText("Applied")).toBeInTheDocument();
    expect(screen.getByDisplayValue("Applied Markdown body")).toBeInTheDocument();
    expect(api.applyReconcile).toHaveBeenCalledWith({
      targetId: "rec-alpha-001",
      expectedVersion: 2,
      backendHash: "sha256:fixture-alpha-001",
      fieldDiff: { body: "Markdown body" },
    });
  });
});

function jsonResponse(body: unknown): Response {
  return new Response(JSON.stringify(body), {
    headers: { "content-type": "application/json" },
  });
}

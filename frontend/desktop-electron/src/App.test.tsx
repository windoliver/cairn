import "@testing-library/jest-dom/vitest";
import { render, screen, waitFor } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { describe, expect, it, vi } from "vitest";
import { App } from "./App";

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
};

describe("App", () => {
  it("renders the vault inspector and loaded record", async () => {
    render(<App api={api} />);

    await waitFor(() => expect(screen.getByText("Desktop Alpha Fixture")).toBeInTheDocument());
    expect(screen.getAllByText("Project memory scaffold").length).toBeGreaterThan(0);
    expect(screen.getByText("Markdown body")).toBeInTheDocument();
    expect(screen.getByText("Graph")).toBeInTheDocument();
    expect(screen.getByText("Lint")).toBeInTheDocument();
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
});

import "@testing-library/jest-dom/vitest";
import { render, screen } from "@testing-library/react";
import { describe, expect, it, vi } from "vitest";
import { VaultSidebar } from "./VaultSidebar";

describe("VaultSidebar", () => {
  it("omits the tag separator when a record has no tags", () => {
    render(
      <VaultSidebar
        vault={{
          id: "desktop-alpha",
          name: "Desktop Alpha Fixture",
          root: "fixtures/desktop-gui-alpha",
          recordCount: 1,
          folderCount: 1,
        }}
        folders={[{ id: "folder-core", name: "Core Memories", parentId: null }]}
        records={[
          {
            id: "rec-alpha-001",
            title: "Project memory scaffold",
            folderId: "folder-core",
            kind: "skill",
            tags: [],
            version: 2,
            confidence: 0.86,
          },
        ]}
        selectedId="rec-alpha-001"
        onSelectRecord={vi.fn()}
      />,
    );

    expect(screen.getByRole("button", { name: /Project memory scaffold/ })).toHaveTextContent(
      "skill · v2",
    );
    expect(screen.getByRole("button", { name: /Project memory scaffold/ })).not.toHaveTextContent(
      /·\s*$/,
    );
  });
});

import "@testing-library/jest-dom/vitest";
import { render, screen } from "@testing-library/react";
import { describe, expect, it } from "vitest";
import { GraphPanel } from "./GraphPanel";

describe("GraphPanel", () => {
  it("renders derived graph nodes and edges as an svg graph", () => {
    render(
      <GraphPanel
        graph={{
          nodes: [
            { id: "rec-alpha-001", label: "Project memory scaffold", kind: "skill", group: "core" },
            { id: "rec-alpha-002", label: "Reconcile review", kind: "procedural", group: "core" },
          ],
          edges: [
            {
              id: "rec-alpha-001--rec-alpha-002",
              source: "rec-alpha-001",
              target: "rec-alpha-002",
              label: "wikilink",
            },
          ],
        }}
      />,
    );

    expect(screen.getByLabelText("Derived graph view")).toBeInTheDocument();
    expect(screen.getByText("Project memory scaffold")).toBeInTheDocument();
    expect(screen.getByText("Reconcile review")).toBeInTheDocument();
    expect(screen.getByText("2 nodes · 1 edges")).toBeInTheDocument();
  });
});

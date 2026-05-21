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
        sessionTree={{
          root: "session-root",
          nodes: [
            {
              id: "session-root",
              parentId: null,
              branchKind: null,
              atTurnId: null,
              toolCallId: null,
              children: ["session-branch"],
            },
            {
              id: "session-branch",
              parentId: "session-root",
              branchKind: "fork",
              atTurnId: "turn-2",
              toolCallId: null,
              children: [],
            },
          ],
          merges: [],
        }}
      />,
    );

    expect(screen.getByLabelText("Derived graph view")).toBeInTheDocument();
    expect(screen.getByText("Project memory scaffold")).toBeInTheDocument();
    expect(screen.getByText("Reconcile review")).toBeInTheDocument();
    expect(screen.getByText("2 nodes · 1 edge")).toBeInTheDocument();
    expect(screen.getByText("Session tree 2 nodes · 0 merges")).toBeInTheDocument();
  });

  it("uses singular labels for a one-node session tree with one merge", () => {
    render(
      <GraphPanel
        graph={{
          nodes: [{ id: "rec-alpha-001", label: "Project memory scaffold", kind: "skill", group: "core" }],
          edges: [],
        }}
        sessionTree={{
          root: "session-root",
          nodes: [
            {
              id: "session-root",
              parentId: null,
              branchKind: null,
              atTurnId: null,
              toolCallId: null,
              children: [],
            },
          ],
          merges: [
            {
              source: "session-root",
              destination: "session-root",
              strategy: "reasoning_summary",
              summaryRecordId: "rec-alpha-002",
              firstTurnId: null,
              lastTurnId: null,
              appliedAtTurnId: "turn-4",
            },
          ],
        }}
      />,
    );

    expect(screen.getByText("1 node · 0 edges")).toBeInTheDocument();
    expect(screen.getByText("Session tree 1 node · 1 merge")).toBeInTheDocument();
  });
});

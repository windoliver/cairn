import "@testing-library/jest-dom/vitest";
import { act, render, screen } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { describe, expect, it, vi } from "vitest";
import { SearchPanel } from "./SearchPanel";

describe("SearchPanel", () => {
  it("shows search request failures inline", async () => {
    const user = userEvent.setup();
    const api = {
      search: vi.fn().mockRejectedValueOnce(new Error("Search service unavailable")),
    };

    render(<SearchPanel api={api} onSelectRecord={vi.fn()} />);

    await user.type(screen.getByLabelText("Search records"), "a");

    expect(await screen.findByText("Search service unavailable")).toBeInTheDocument();
    expect(api.search).toHaveBeenCalledWith("a");
  });

  it("keeps the newest search results when responses arrive out of order", async () => {
    const user = userEvent.setup();
    let resolveFirst: (value: Array<{ recordId: string; title: string; snippet: string; score: number }>) => void;
    const firstSearch = new Promise<Array<{ recordId: string; title: string; snippet: string; score: number }>>(
      (resolve) => {
        resolveFirst = resolve;
      },
    );
    const api = {
      search: vi
        .fn()
        .mockReturnValueOnce(firstSearch)
        .mockResolvedValueOnce([
          { recordId: "rec-alpha-002", title: "Fresh result", snippet: "fresh", score: 1 },
        ]),
    };

    render(<SearchPanel api={api} onSelectRecord={vi.fn()} />);

    const input = screen.getByLabelText("Search records");
    await user.type(input, "a");
    await user.type(input, "b");

    expect(await screen.findByText("Fresh result")).toBeInTheDocument();

    resolveFirst!([{ recordId: "rec-alpha-001", title: "Stale result", snippet: "stale", score: 1 }]);

    expect(await screen.findByText("Fresh result")).toBeInTheDocument();
    expect(screen.queryByText("Stale result")).not.toBeInTheDocument();
  });

  it("clears stale results while a newer search is pending", async () => {
    const user = userEvent.setup();
    let resolveSecond: (value: Array<{ recordId: string; title: string; snippet: string; score: number }>) => void;
    const secondSearch = new Promise<Array<{ recordId: string; title: string; snippet: string; score: number }>>(
      (resolve) => {
        resolveSecond = resolve;
      },
    );
    const api = {
      search: vi
        .fn()
        .mockResolvedValueOnce([
          { recordId: "rec-alpha-001", title: "Old result", snippet: "old", score: 1 },
        ])
        .mockReturnValueOnce(secondSearch),
    };

    render(<SearchPanel api={api} onSelectRecord={vi.fn()} />);

    const input = screen.getByLabelText("Search records");
    await user.type(input, "a");
    expect(await screen.findByText("Old result")).toBeInTheDocument();

    await user.type(input, "b");

    expect(screen.queryByText("Old result")).not.toBeInTheDocument();

    await act(async () => {
      resolveSecond!([{ recordId: "rec-alpha-002", title: "Fresh result", snippet: "fresh", score: 1 }]);
    });

    expect(await screen.findByText("Fresh result")).toBeInTheDocument();
  });
});

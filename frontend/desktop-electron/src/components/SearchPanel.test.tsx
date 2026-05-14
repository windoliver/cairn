import "@testing-library/jest-dom/vitest";
import { render, screen } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { describe, expect, it, vi } from "vitest";
import { SearchPanel } from "./SearchPanel";

describe("SearchPanel", () => {
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
});

import { useState } from "react";
import type { DesktopApi } from "../App";
import type { DesktopSearchResult } from "../api/types";

export function SearchPanel({
  api,
  onSelectRecord,
}: {
  api: DesktopApi;
  onSelectRecord: (id: string) => void;
}) {
  const [query, setQuery] = useState("");
  const [results, setResults] = useState<DesktopSearchResult[]>([]);

  async function runSearch(nextQuery: string) {
    setQuery(nextQuery);
    setResults(nextQuery.trim() ? await api.search(nextQuery) : []);
  }

  return (
    <section className="panel">
      <h2>Search</h2>
      <input
        aria-label="Search records"
        value={query}
        onChange={(event) => void runSearch(event.target.value)}
      />
      {results.map((result) => (
        <button key={result.recordId} type="button" onClick={() => onSelectRecord(result.recordId)}>
          {result.title}
        </button>
      ))}
    </section>
  );
}

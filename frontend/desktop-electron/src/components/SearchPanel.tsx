import { useRef, useState } from "react";
import type { DesktopApi } from "../App";
import type { DesktopSearchResult } from "../api/types";

type SearchApi = Pick<DesktopApi, "search">;

export function SearchPanel({
  api,
  onSelectRecord,
}: {
  api: SearchApi;
  onSelectRecord: (id: string) => void;
}) {
  const [query, setQuery] = useState("");
  const [results, setResults] = useState<DesktopSearchResult[]>([]);
  const searchSequence = useRef(0);

  async function runSearch(nextQuery: string) {
    setQuery(nextQuery);
    const sequence = searchSequence.current + 1;
    searchSequence.current = sequence;
    if (!nextQuery.trim()) {
      setResults([]);
      return;
    }
    const nextResults = await api.search(nextQuery);
    if (searchSequence.current === sequence) {
      setResults(nextResults);
    }
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

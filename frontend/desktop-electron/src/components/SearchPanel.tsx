import { useRef, useState } from "react";
import type { DesktopApi } from "../App";
import type { DesktopSearchResult } from "../api/types";

type SearchApi = Pick<DesktopApi, "search">;

export function SearchPanel({
  api,
  selectedId,
  onSelectRecord,
}: {
  api: SearchApi;
  selectedId?: string | null;
  onSelectRecord: (id: string) => void;
}) {
  const [query, setQuery] = useState("");
  const [results, setResults] = useState<DesktopSearchResult[]>([]);
  const [error, setError] = useState<string | null>(null);
  const searchSequence = useRef(0);

  async function runSearch(nextQuery: string) {
    setQuery(nextQuery);
    const sequence = searchSequence.current + 1;
    searchSequence.current = sequence;
    if (!nextQuery.trim()) {
      setResults([]);
      setError(null);
      return;
    }
    setResults([]);
    setError(null);
    try {
      const nextResults = await api.search(nextQuery);
      if (searchSequence.current === sequence) {
        setResults(nextResults);
        setError(null);
      }
    } catch (error) {
      if (searchSequence.current === sequence) {
        setResults([]);
        setError(error instanceof Error ? error.message : "Search request failed");
      }
    }
  }

  return (
    <section className="panel">
      <h2>Search</h2>
      <input
        aria-label="Search records"
        value={query}
        onChange={(event) => void runSearch(event.target.value)}
        onKeyDown={(event) => {
          if (event.key === "Enter" && results[0]) {
            onSelectRecord(results[0].recordId);
          }
        }}
      />
      {error && <p>{error}</p>}
      {query.trim() && !error && results.length > 0 && (
        <p className="searchStatus">
          {results.length} {results.length === 1 ? "result" : "results"}
        </p>
      )}
      {results.map((result) => (
        <button
          aria-current={result.recordId === selectedId ? "true" : undefined}
          className={result.recordId === selectedId ? "searchResult selected" : "searchResult"}
          key={result.recordId}
          type="button"
          onClick={() => onSelectRecord(result.recordId)}
        >
          <span>{result.title}</span>
          <small>{result.snippet}</small>
        </button>
      ))}
    </section>
  );
}

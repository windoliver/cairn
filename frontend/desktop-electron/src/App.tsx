import { useEffect, useMemo, useRef, useState } from "react";
import { DesktopApiClient } from "./api/client";
import type {
  DesktopFolder,
  DesktopGraph,
  DesktopLintFinding,
  DesktopRecordDetail,
  DesktopRecordSummary,
  DesktopVaultSummary,
} from "./api/types";
import { GraphPanel } from "./components/GraphPanel";
import { LintPanel } from "./components/LintPanel";
import { RecordDetail } from "./components/RecordDetail";
import { SearchPanel } from "./components/SearchPanel";
import { VaultSidebar } from "./components/VaultSidebar";
import "./styles.css";

export type DesktopApi = Pick<
  DesktopApiClient,
  | "vault"
  | "folders"
  | "records"
  | "record"
  | "graph"
  | "lint"
  | "search"
  | "previewReconcile"
  | "applyReconcile"
>;

type AppState = {
  vault: DesktopVaultSummary | null;
  folders: DesktopFolder[];
  records: DesktopRecordSummary[];
  selected: DesktopRecordDetail | null;
  graph: DesktopGraph | null;
  lint: DesktopLintFinding[];
  error: string | null;
};

export function App({
  api: providedApi,
}: {
  api?: DesktopApi;
}) {
  const defaultApi = useMemo(() => new DesktopApiClient(resolveDesktopApiBaseUrl()), []);
  const api = providedApi ?? defaultApi;

  const [state, setState] = useState<AppState>({
    vault: null,
    folders: [],
    records: [],
    selected: null,
    graph: null,
    lint: [],
    error: null,
  });
  const selectionSequence = useRef(0);

  useEffect(() => {
    let cancelled = false;
    async function load() {
      try {
        const [vault, folders, records, graph, lint] = await Promise.all([
          api.vault(),
          api.folders(),
          api.records(),
          api.graph(),
          api.lint(),
        ]);
        let selected: DesktopRecordDetail | null = null;
        let error: string | null = null;
        if (records[0]) {
          try {
            selected = await api.record(records[0].id);
          } catch (recordError) {
            error =
              recordError instanceof Error ? recordError.message : "Failed to load record detail";
          }
        }
        if (!cancelled) {
          setState({ vault, folders, records, selected, graph, lint, error });
        }
      } catch (error) {
        if (!cancelled) {
          setState((current) => ({
            ...current,
            error: error instanceof Error ? error.message : "Failed to load desktop data",
          }));
        }
      }
    }
    void load();
    return () => {
      cancelled = true;
    };
  }, [api]);

  const selectedId = state.selected?.id ?? null;
  const recordsByFolder = useMemo(() => state.records, [state.records]);

  async function selectRecord(id: string) {
    const sequence = selectionSequence.current + 1;
    selectionSequence.current = sequence;
    try {
      const selected = await api.record(id);
      if (selectionSequence.current === sequence) {
        setState((current) => ({ ...current, selected, error: null }));
      }
    } catch (error) {
      if (selectionSequence.current === sequence) {
        setState((current) => ({
          ...current,
          error: error instanceof Error ? error.message : "Failed to load record detail",
        }));
      }
    }
  }

  function applyRecord(record: DesktopRecordDetail) {
    selectionSequence.current += 1;
    setState((current) => ({
      ...current,
      records: current.records.map((summary) =>
        summary.id === record.id ? recordToSummary(record) : summary,
      ),
      selected: record,
      error: null,
    }));
  }

  return (
    <main className="app">
      {state.error && <p className="appErrorBanner">{state.error}</p>}
      <VaultSidebar
        vault={state.vault}
        folders={state.folders}
        records={recordsByFolder}
        selectedId={selectedId}
        onSelectRecord={(id) => void selectRecord(id)}
      />
      <section className="workspace">
        <RecordDetail
          record={state.selected}
          api={api}
          onRecordApplied={applyRecord}
        />
        <div className="lowerPanels">
          <GraphPanel graph={state.graph} />
          <SearchPanel api={api} onSelectRecord={(id) => void selectRecord(id)} />
          <LintPanel findings={state.lint} />
        </div>
      </section>
    </main>
  );
}

export function resolveDesktopApiBaseUrl(): string {
  return window.cairnDesktop?.apiBaseUrl ?? import.meta.env.VITE_CAIRN_DESKTOP_API ?? "http://127.0.0.1:4000";
}

function recordToSummary(record: DesktopRecordDetail): DesktopRecordSummary {
  return {
    id: record.id,
    title: record.title,
    folderId: record.folderId,
    kind: record.kind,
    tags: record.tags,
    version: record.version,
    confidence: record.confidence,
  };
}

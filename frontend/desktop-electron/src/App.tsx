import { useEffect, useMemo, useState } from "react";
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
  api = new DesktopApiClient(resolveDesktopApiBaseUrl()),
}: {
  api?: DesktopApi;
}) {
  const [state, setState] = useState<AppState>({
    vault: null,
    folders: [],
    records: [],
    selected: null,
    graph: null,
    lint: [],
    error: null,
  });

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
        const selected = records[0] ? await api.record(records[0].id) : null;
        if (!cancelled) {
          setState({ vault, folders, records, selected, graph, lint, error: null });
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
    const selected = await api.record(id);
    setState((current) => ({ ...current, selected }));
  }

  if (state.error) {
    return <main className="app appError">{state.error}</main>;
  }

  return (
    <main className="app">
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
          onRecordApplied={(record) => setState((current) => ({ ...current, selected: record }))}
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

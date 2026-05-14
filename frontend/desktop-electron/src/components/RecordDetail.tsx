import { useState } from "react";
import type { DesktopApi } from "../App";
import type { DesktopRecordDetail } from "../api/types";
import { ReconcilePanel } from "./ReconcilePanel";

export function RecordDetail({
  record,
  api,
  onRecordApplied,
}: {
  record: DesktopRecordDetail | null;
  api: DesktopApi;
  onRecordApplied: (record: DesktopRecordDetail) => void;
}) {
  const [draft, setDraft] = useState("");

  if (!record) {
    return <section className="recordDetail">Loading record...</section>;
  }

  const body = draft || record.body;

  return (
    <section className="recordDetail">
      <header>
        <h2>{record.title}</h2>
        <div className="metaLine">
          <span>{record.kind}</span>
          <span>v{record.version}</span>
          <span>{Math.round(record.confidence * 100)}%</span>
        </div>
      </header>
      <textarea
        aria-label="Record body"
        value={body}
        onChange={(event) => setDraft(event.target.value)}
      />
      <ReconcilePanel
        api={api}
        record={record}
        draftBody={body}
        onRecordApplied={onRecordApplied}
      />
    </section>
  );
}

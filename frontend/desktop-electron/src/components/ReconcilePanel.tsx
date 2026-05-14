import { useState } from "react";
import type { DesktopApi } from "../App";
import type { DesktopRecordDetail, DesktopReconcilePreview } from "../api/types";

type PreviewState = {
  key: string;
  preview: DesktopReconcilePreview;
};

export function ReconcilePanel({
  api,
  record,
  draftBody,
  onRecordApplied,
}: {
  api: DesktopApi;
  record: DesktopRecordDetail;
  draftBody: string;
  onRecordApplied: (record: DesktopRecordDetail) => void;
}) {
  const [previewState, setPreviewState] = useState<PreviewState | null>(null);
  const [applyStatus, setApplyStatus] = useState<string | null>(null);
  const currentRequestKey = requestKey(record, draftBody);
  const preview = previewState?.key === currentRequestKey ? previewState.preview : null;

  function request() {
    return {
      targetId: record.id,
      expectedVersion: record.version,
      backendHash: record.backendHash,
      fieldDiff: { body: draftBody },
    };
  }

  async function review() {
    const next = await api.previewReconcile(request());
    setPreviewState({ key: currentRequestKey, preview: next });
    setApplyStatus(null);
  }

  async function apply() {
    const result = await api.applyReconcile(request());
    if (result.accepted && result.record) {
      onRecordApplied(result.record);
      setApplyStatus("Applied");
    } else {
      setApplyStatus(result.rejectedFields.map((field) => field.message).join(", "));
    }
  }

  return (
    <section className="reconcilePanel">
      <button type="button" onClick={() => void review()}>
        Review reconcile
      </button>
      {preview && (
        <>
          <p>
            {preview.accepted
              ? "Ready to apply"
              : preview.rejectedFields.map((field) => field.message).join(", ")}
          </p>
          {preview.accepted && (
            <button type="button" onClick={() => void apply()}>
              Apply reconcile
            </button>
          )}
        </>
      )}
      {applyStatus && <p>{applyStatus}</p>}
    </section>
  );
}

function requestKey(record: DesktopRecordDetail, draftBody: string): string {
  return JSON.stringify([record.id, record.version, record.backendHash, draftBody]);
}

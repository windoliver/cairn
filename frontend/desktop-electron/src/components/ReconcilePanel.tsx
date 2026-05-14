import { useState } from "react";
import type { DesktopApi } from "../App";
import type { DesktopRecordDetail, DesktopReconcilePreview } from "../api/types";

type PreviewState = {
  key: string;
  preview: DesktopReconcilePreview;
};

type RequestErrorState = {
  key: string;
  message: string;
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
  const [requestErrorState, setRequestErrorState] = useState<RequestErrorState | null>(null);
  const currentRequestKey = requestKey(record, draftBody);
  const preview = previewState?.key === currentRequestKey ? previewState.preview : null;
  const requestError =
    requestErrorState?.key === currentRequestKey ? requestErrorState.message : null;

  function request() {
    return {
      targetId: record.id,
      expectedVersion: record.version,
      backendHash: record.backendHash,
      fieldDiff: { body: draftBody },
    };
  }

  async function review() {
    try {
      const next = await api.previewReconcile(request());
      setPreviewState({ key: currentRequestKey, preview: next });
      setApplyStatus(null);
      setRequestErrorState(null);
    } catch (error) {
      setPreviewState(null);
      setApplyStatus(null);
      setRequestErrorState({ key: currentRequestKey, message: errorMessage(error) });
    }
  }

  async function apply() {
    try {
      const result = await api.applyReconcile(request());
      if (result.accepted && result.record) {
        onRecordApplied(result.record);
        setPreviewState(null);
        setApplyStatus("Applied");
      } else {
        setApplyStatus(
          result.rejectedFields.map((field) => field.message).join(", ") ||
            "Reconcile apply rejected",
        );
      }
      setRequestErrorState(null);
    } catch (error) {
      setApplyStatus(null);
      setRequestErrorState({ key: currentRequestKey, message: errorMessage(error) });
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
      {requestError && <p>{requestError}</p>}
      {applyStatus && <p>{applyStatus}</p>}
    </section>
  );
}

function requestKey(record: DesktopRecordDetail, draftBody: string): string {
  return JSON.stringify([record.id, record.version, record.backendHash, draftBody]);
}

function errorMessage(error: unknown): string {
  return error instanceof Error ? error.message : "Desktop reconcile request failed";
}

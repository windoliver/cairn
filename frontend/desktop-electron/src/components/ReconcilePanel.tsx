import { useRef, useState } from "react";
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

type ApplyStatusState = {
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
  const [applyStatusState, setApplyStatusState] = useState<ApplyStatusState | null>(null);
  const [requestErrorState, setRequestErrorState] = useState<RequestErrorState | null>(null);
  const currentRequestKey = requestKey(record, draftBody);
  const latestRequestKey = useRef(currentRequestKey);
  const latestReviewSequence = useRef(0);
  const latestApplySequence = useRef(0);
  latestRequestKey.current = currentRequestKey;
  const preview = previewState?.key === currentRequestKey ? previewState.preview : null;
  const requestError =
    requestErrorState?.key === currentRequestKey ? requestErrorState.message : null;
  const applyStatus =
    applyStatusState?.key === currentRequestKey ? applyStatusState.message : null;

  function request() {
    return {
      targetId: record.id,
      expectedVersion: record.version,
      backendHash: record.backendHash,
      fieldDiff: { body: draftBody },
    };
  }

  async function review() {
    const sequence = latestReviewSequence.current + 1;
    latestReviewSequence.current = sequence;
    try {
      const next = await api.previewReconcile(request());
      if (
        latestReviewSequence.current !== sequence ||
        latestRequestKey.current !== currentRequestKey
      ) {
        return;
      }
      setPreviewState({ key: currentRequestKey, preview: next });
      setApplyStatusState(null);
      setRequestErrorState(null);
    } catch (error) {
      if (
        latestReviewSequence.current !== sequence ||
        latestRequestKey.current !== currentRequestKey
      ) {
        return;
      }
      setPreviewState(null);
      setApplyStatusState(null);
      setRequestErrorState({ key: currentRequestKey, message: errorMessage(error) });
    }
  }

  async function apply() {
    const sequence = latestApplySequence.current + 1;
    latestApplySequence.current = sequence;
    try {
      const result = await api.applyReconcile(request());
      if (
        latestApplySequence.current !== sequence ||
        latestRequestKey.current !== currentRequestKey
      ) {
        return;
      }
      if (result.accepted && result.record) {
        onRecordApplied(result.record);
        setPreviewState(null);
        setApplyStatusState({
          key: requestKey(result.record, result.record.body),
          message: "Applied",
        });
      } else {
        setPreviewState(null);
        setApplyStatusState({
          key: currentRequestKey,
          message:
            result.rejectedFields.map((field) => field.message).join(", ") ||
            "Reconcile apply rejected",
        });
      }
      setRequestErrorState(null);
    } catch (error) {
      if (
        latestApplySequence.current !== sequence ||
        latestRequestKey.current !== currentRequestKey
      ) {
        return;
      }
      setApplyStatusState(null);
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

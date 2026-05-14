import { useState } from "react";
import type { DesktopApi } from "../App";
import type { DesktopRecordDetail, DesktopReconcilePreview } from "../api/types";

export function ReconcilePanel({
  api,
  record,
  draftBody,
}: {
  api: DesktopApi;
  record: DesktopRecordDetail;
  draftBody: string;
}) {
  const [preview, setPreview] = useState<DesktopReconcilePreview | null>(null);

  async function review() {
    const next = await api.previewReconcile({
      targetId: record.id,
      expectedVersion: record.version,
      backendHash: record.backendHash,
      fieldDiff: { body: draftBody },
    });
    setPreview(next);
  }

  return (
    <section className="reconcilePanel">
      <button type="button" onClick={() => void review()}>
        Review reconcile
      </button>
      {preview && (
        <p>
          {preview.accepted
            ? "Ready to apply"
            : preview.rejectedFields.map((field) => field.message).join(", ")}
        </p>
      )}
    </section>
  );
}

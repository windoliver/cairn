import type { DesktopFolder, DesktopRecordSummary, DesktopVaultSummary } from "../api/types";

export function VaultSidebar({
  vault,
  folders,
  records,
  selectedId,
  onSelectRecord,
}: {
  vault: DesktopVaultSummary | null;
  folders: DesktopFolder[];
  records: DesktopRecordSummary[];
  selectedId: string | null;
  onSelectRecord: (id: string) => void;
}) {
  return (
    <aside className="sidebar">
      <h1>{vault?.name ?? "Loading vault"}</h1>
      <p>{vault ? `${vault.recordCount} records · ${vault.folderCount} folders` : "Connecting"}</p>
      {folders.map((folder) => (
        <section key={folder.id} className="folderGroup">
          <h2>{folder.name}</h2>
          {records
            .filter((record) => record.folderId === folder.id)
            .map((record) => (
              <button
                className={record.id === selectedId ? "recordButton selected" : "recordButton"}
                key={record.id}
                type="button"
                onClick={() => onSelectRecord(record.id)}
              >
                <span>{record.title}</span>
                <small>{recordSummaryMeta(record)}</small>
              </button>
            ))}
        </section>
      ))}
    </aside>
  );
}

function recordSummaryMeta(record: DesktopRecordSummary): string {
  return [record.kind, `v${record.version}`, record.tags.join(", ")]
    .filter(Boolean)
    .join(" · ");
}

import {
  Activity,
  AlertTriangle,
  CheckCircle2,
  DatabaseZap,
  Gauge,
  Search,
} from "lucide-react";
import type { ReactNode } from "react";
import type { DesktopSreReport, SreStatus } from "../api/types";

export function SreWorkspace({
  report,
  error,
}: {
  report: DesktopSreReport | null;
  error?: string | null;
}) {
  if (!report) {
    return (
      <section className="sreWorkspace">
        <h2>{error ? "SRE report unavailable" : "SRE report loading"}</h2>
        {error && <p className="srePanelLead">The SRE endpoint could not be loaded.</p>}
      </section>
    );
  }

  return (
    <section className="sreWorkspace">
      <header className="sreHeader">
        <div>
          <h2>SRE</h2>
          <span>{report.vault.name}</span>
        </div>
        <StatusBadge status={report.gates.status} />
      </header>

      <div className="sreSummaryStrip">
        <StatusCard
          icon={<Activity size={18} />}
          title="Workflow"
          status={report.workflow.status}
          detail={`${report.workflow.dead_letter_count} dead-letter`}
        />
        <StatusCard
          icon={<Gauge size={18} />}
          title="Rehydration"
          status={report.rehydration.status}
          detail={`${formatMs(report.rehydration.p95_latency_ms)} / ${formatMs(report.rehydration.slo_ms)}`}
        />
        <StatusCard
          icon={<DatabaseZap size={18} />}
          title="Projection"
          status={report.projection.status}
          detail={report.projection.nexus_state}
        />
        <StatusCard
          icon={<Search size={18} />}
          title="Search"
          status={report.search.status}
          detail={`${report.search.modes.length} modes`}
        />
      </div>

      <div className="srePanelGrid">
        <section className="srePanel">
          <h3>Workflow Jobs</h3>
          <table>
            <thead>
              <tr>
                <th>Kind</th>
                <th>Queued</th>
                <th>Leased</th>
                <th>Oldest</th>
                <th>Status</th>
              </tr>
            </thead>
            <tbody>
              {report.workflow.kinds.map((kind) => (
                <tr key={kind.kind}>
                  <td>{kind.kind}</td>
                  <td>{kind.queued}</td>
                  <td>{kind.leased}</td>
                  <td>{formatMs(kind.oldest_queued_age_ms)}</td>
                  <td>
                    <StatusBadge status={kind.status} />
                  </td>
                </tr>
              ))}
            </tbody>
          </table>
        </section>

        <section className="srePanel">
          <h3>Rehydration Latency</h3>
          <MetricRow label="p95" value={formatMs(report.rehydration.p95_latency_ms)} />
          <MetricRow label="latest" value={formatMs(report.rehydration.latest_latency_ms)} />
          <MetricRow label="samples" value={String(report.rehydration.sample_count)} />
          {report.rehydration.last_gate && (
            <MetricRow label="last gate" value={report.rehydration.last_gate.name} />
          )}
        </section>

        <section className="srePanel">
          <h3>Projection Targets</h3>
          <p className="srePanelLead">
            {report.projection.nexus_state}
            {report.projection.nexus_reason ? ` · ${report.projection.nexus_reason}` : ""}
          </p>
          {report.projection.targets.map((target) => (
            <div className="sreMetricRow" key={target.target}>
              <span>{target.target}</span>
              <span>
                {target.stale} stale · {target.failed} failed
              </span>
              <StatusBadge status={target.status} />
            </div>
          ))}
        </section>

        <section className="srePanel">
          <h3>Search Modes</h3>
          {report.search.modes.map((mode) => (
            <div className="sreMetricRow" key={mode.mode}>
              <span>{mode.mode}</span>
              <span>
                {mode.degraded}/{mode.invocations} degraded
              </span>
              <StatusBadge status={mode.status} />
            </div>
          ))}
        </section>

        <section className="srePanel srePanelWide">
          <h3>Release Gates</h3>
          {report.gates.gates.map((gate) => (
            <div className="sreMetricRow" key={gate.name}>
              <span>{gate.name}</span>
              <span>
                {formatMeasurement(gate.measured)} {gate.unit}
              </span>
              <StatusBadge status={gate.status} />
            </div>
          ))}
        </section>
      </div>
    </section>
  );
}

function StatusCard({
  icon,
  title,
  status,
  detail,
}: {
  icon: ReactNode;
  title: string;
  status: SreStatus;
  detail: string;
}) {
  const StatusIcon = status === "ok" ? CheckCircle2 : AlertTriangle;
  return (
    <div className={`sreStatusCard status-${status}`}>
      <span aria-hidden="true">{icon}</span>
      <div>
        <h3>{title}</h3>
        <p>{detail}</p>
      </div>
      <div className="sreCardStatus">
        <StatusBadge status={status} />
        <StatusIcon size={16} aria-hidden="true" />
      </div>
    </div>
  );
}

function StatusBadge({ status }: { status: SreStatus }) {
  return <span className={`sreStatusBadge status-${status}`}>{status}</span>;
}

function MetricRow({ label, value }: { label: string; value: string }) {
  return (
    <div className="sreMetricRow">
      <span>{label}</span>
      <span>{value}</span>
      <span />
    </div>
  );
}

function formatMs(value: number | null): string {
  if (value === null) {
    return "unknown";
  }
  return `${Math.round(value)}ms`;
}

function formatMeasurement(value: number | null): string {
  if (value === null) {
    return "unknown";
  }
  return Number.isInteger(value) ? String(value) : value.toFixed(1);
}

import type { DesktopLintFinding } from "../api/types";

export function LintPanel({ findings }: { findings: DesktopLintFinding[] }) {
  return (
    <section className="panel">
      <h2>Lint</h2>
      {findings.map((finding) => (
        <article key={finding.id} className="lintFinding">
          <strong>{finding.severity}</strong>
          <p>{finding.message}</p>
        </article>
      ))}
    </section>
  );
}

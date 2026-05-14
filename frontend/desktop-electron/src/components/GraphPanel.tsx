import type { DesktopGraph } from "../api/types";

export function GraphPanel({ graph }: { graph: DesktopGraph | null }) {
  return (
    <section className="panel">
      <h2>Graph</h2>
      <p>{graph ? `${graph.nodes.length} nodes · ${graph.edges.length} edges` : "Loading graph"}</p>
      <div className="graphList">
        {graph?.edges.map((edge) => (
          <span key={edge.id}>
            {edge.source} -&gt; {edge.target}
          </span>
        ))}
      </div>
    </section>
  );
}

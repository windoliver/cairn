import type { DesktopGraph, DesktopSessionTree } from "../api/types";

export function GraphPanel({
  graph,
  sessionTree,
}: {
  graph: DesktopGraph | null;
  sessionTree?: DesktopSessionTree | null;
}) {
  const layout = graph ? layoutGraph(graph) : null;

  return (
    <section className="panel">
      <h2>Graph</h2>
      <p>{graph ? `${countLabel(graph.nodes.length, "node")} · ${countLabel(graph.edges.length, "edge")}` : "Loading graph"}</p>
      {sessionTree && (
        <p className="panelMeta">
          Session tree {countLabel(sessionTree.nodes.length, "node")} · {countLabel(sessionTree.merges.length, "merge")}
        </p>
      )}
      {layout && (
        <svg aria-label="Derived graph view" className="graphCanvas" viewBox="0 0 320 180" role="img">
          {layout.edges.map((edge) => (
            <line
              className="graphEdge"
              key={edge.id}
              x1={edge.source.x}
              x2={edge.target.x}
              y1={edge.source.y}
              y2={edge.target.y}
            />
          ))}
          {layout.nodes.map((node) => (
            <g key={node.id}>
              <circle className={`graphNode graphNode-${node.kind}`} cx={node.x} cy={node.y} r="16" />
              <text className="graphLabel" x={node.x} y={node.y + 31} textAnchor="middle">
                {node.label}
              </text>
            </g>
          ))}
        </svg>
      )}
    </section>
  );
}

function countLabel(count: number, singular: string) {
  return `${count} ${count === 1 ? singular : `${singular}s`}`;
}

function layoutGraph(graph: DesktopGraph) {
  const centerX = 160;
  const centerY = 76;
  const radius = Math.min(96, 28 + graph.nodes.length * 18);
  const nodes = graph.nodes.map((node, index) => {
    const angle = graph.nodes.length === 1 ? 0 : (Math.PI * 2 * index) / graph.nodes.length;
    return {
      ...node,
      x: Math.round(centerX + Math.cos(angle) * radius),
      y: Math.round(centerY + Math.sin(angle) * radius * 0.62),
    };
  });
  const byId = new Map(nodes.map((node) => [node.id, node]));
  const edges = graph.edges.flatMap((edge) => {
    const source = byId.get(edge.source);
    const target = byId.get(edge.target);
    return source && target ? [{ ...edge, source, target }] : [];
  });
  return { nodes, edges };
}

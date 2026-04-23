import { vi } from "vitest";

/**
 * Cytoscape test double.
 *
 * The real `cytoscape` package touches the DOM and layout internals that jsdom
 * can't satisfy. This mock captures what each instance was configured with
 * (elements, style, layout, event handlers) so tests can assert on it without
 * actually rendering a graph. Each call to the mocked `cytoscape()` pushes a
 * new {@link CapturedCy} record onto the shared {@link instances} array.
 */
export interface CapturedCy {
  container: HTMLElement;
  elements: unknown[];
  style: unknown;
  layout: { name: string; [k: string]: unknown };
  handlers: Record<string, (evt: { target: { id: () => string } }) => void>;
  destroyed: boolean;
  // NEW (T53c I4):
  /** Per-node id → data map; mutations from `cy.getElementById(id).data(key, value)` land here. */
  nodeData: Map<string, Record<string, unknown>>;
  /** Incremented whenever `cy.layout({...}).run()` is called post-mount; initialised to 1 for the mount layout. */
  layoutCalls: number;
}

export const instances: CapturedCy[] = [];

interface FakeCytoscapeConfig {
  container: HTMLElement;
  elements: Array<{ data: Record<string, unknown>; classes?: string }>;
  style?: unknown;
  layout?: { name: string; [k: string]: unknown };
}

function fakeCytoscape(config: FakeCytoscapeConfig) {
  // Seed nodeData from the initial elements so the initial labels are queryable.
  const nodeData = new Map<string, Record<string, unknown>>();
  for (const el of config.elements ?? []) {
    if (el.data && typeof el.data.id === "string" && !("source" in el.data)) {
      nodeData.set(el.data.id, { ...el.data });
    }
  }

  const captured: CapturedCy = {
    container: config.container,
    elements: config.elements,
    style: config.style,
    layout: config.layout ?? { name: "grid" },
    handlers: {},
    destroyed: false,
    nodeData,
    // Mount runs the initial layout once.
    layoutCalls: 1,
  };
  instances.push(captured);
  return {
    on(event: string, _selector: string, handler: CapturedCy["handlers"][string]) {
      captured.handlers[event] = handler;
    },
    destroy() {
      captured.destroyed = true;
    },
    getElementById(id: string) {
      if (!captured.nodeData.has(id)) captured.nodeData.set(id, {});
      // The `.has()` guard above guarantees the key exists; the fallback
      // `{}` is a safety net that is never reached in practice.
      const entry = captured.nodeData.get(id) ?? {};
      return {
        data(key: string, value: unknown = undefined) {
          if (value !== undefined) {
            entry[key] = value;
            return undefined;
          }
          return entry[key];
        },
      };
    },
    layout(_opts: unknown) {
      return {
        run() {
          captured.layoutCalls += 1;
        },
      };
    },
  };
}

// `cytoscape.use` is called at module load time to register the dagre plugin;
// expose a no-op spy so the component's `cytoscape.use(dagre)` doesn't throw.
(fakeCytoscape as unknown as { use: ReturnType<typeof vi.fn> }).use = vi.fn();

vi.mock("cytoscape", () => ({ default: fakeCytoscape }));
vi.mock("cytoscape-dagre", () => ({ default: () => {} }));

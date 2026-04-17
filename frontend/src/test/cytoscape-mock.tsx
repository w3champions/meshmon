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
}

export const instances: CapturedCy[] = [];

interface FakeCytoscapeConfig {
  container: HTMLElement;
  elements: unknown[];
  style?: unknown;
  layout?: { name: string; [k: string]: unknown };
}

function fakeCytoscape(config: FakeCytoscapeConfig) {
  const captured: CapturedCy = {
    container: config.container,
    elements: config.elements,
    style: config.style,
    layout: config.layout ?? { name: "grid" },
    handlers: {},
    destroyed: false,
  };
  instances.push(captured);
  return {
    on(event: string, _selector: string, handler: CapturedCy["handlers"][string]) {
      captured.handlers[event] = handler;
    },
    destroy() {
      captured.destroyed = true;
    },
  };
}

// `cytoscape.use` is called at module load time to register the dagre plugin;
// expose a no-op spy so the component's `cytoscape.use(dagre)` doesn't throw.
(fakeCytoscape as unknown as { use: ReturnType<typeof vi.fn> }).use = vi.fn();

vi.mock("cytoscape", () => ({ default: fakeCytoscape }));
vi.mock("cytoscape-dagre", () => ({ default: () => {} }));

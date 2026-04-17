// Ambient declaration for `cytoscape-dagre` which ships no types.
// It's a standard cytoscape extension: export default is the register function
// that `cytoscape.use(...)` receives.
declare module "cytoscape-dagre" {
  import type cytoscape from "cytoscape";
  const ext: cytoscape.Ext;
  export default ext;
}

# meshmon frontend

React 19 SPA served as static assets by the meshmon service.

## Develop

```bash
npm install
npm run dev    # http://localhost:5173, proxies /api to http://localhost:8080
```

Requires a running service on `:8080`. See the workspace README for how to start it.

## Build

```bash
npm run build  # dist/ is embedded into the service binary
```

## Layout

- `src/api/` — openapi-fetch client + generated types. Files matching `*.gen.*` (`openapi.gen.json`, `schema.gen.ts`) are committed and regenerated on every script entry point; never hand-edited.
- `src/components/ui/` — shadcn primitives (copy-pasted from `npx shadcn add`).
- `src/components/layout/` — app-shell chrome (bar, drawer, menus).
- `src/pages/` — route components.
- `src/router/` — TanStack Router route tree (code-based).
- `src/stores/` — Zustand stores (auth, ui, toast).
- `src/styles/globals.css` — Tailwind v4 `@theme` tokens + print rules.

## Regenerate the API schema

After backend handler changes, regenerate both committed artifacts:

```bash
cargo xtask openapi                    # writes frontend/src/api/openapi.gen.json
cd frontend && npm run openapi:types   # writes frontend/src/api/schema.gen.ts
```

Every `npm run …` entry point runs `openapi:types` first, so the TS regeneration is automatic in dev flow. CI verifies drift with `git diff --exit-code` on both files. Never hand-edit files matching `*.gen.*`.

## Test

```bash
npm run test        # vitest run
npm run test:watch
```

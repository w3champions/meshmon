# Results tabs

Per-tab sub-components mounted inside `/campaigns/:id`. The tab shell lives in
`src/pages/CampaignDetail.tsx`; URL param `?tab=` drives the active tab.

## Tabs

| Component | Tab | Role |
|---|---|---|
| `CandidatesTab.tsx` | `candidates` (default) | KPI strip + ranked candidate list + drilldown dialog. |
| `PairsTab.tsx` | `pairs` | One row per baseline pair with row-level force + detail actions. |
| `RawTab.tsx` | `raw` | Every measurement attributed to the campaign, virtualised, with filter chips. |
| `SettingsTab.tsx` | `settings` | Evaluator knobs form with a **Re-evaluate** action. Gated on `completed` / `evaluated`. |

## Supporting components

| File | Role |
|---|---|
| `CandidateTable.tsx` | Sortable table of `EvaluationCandidateDto` rows used by the Candidates tab. |
| `CandidatesTabParts.tsx` | Shared presentational helpers (KPI pills, empty / loading / error states). |
| `DrilldownDialog.tsx` | Per-candidate centered modal with paginated pair-detail rows, sticky filter toolbar, and inline `MtrPanel` for MTR drilldowns. |
| `CandidatePairTable.tsx` | Virtualized table inside the drilldown dialog. Mirrors `RawTab.tsx`'s scroll-append recipe. |
| `CandidatePairFilters.tsx` | Sticky toolbar inside the drilldown dialog: numeric runtime filters + qualifies-only switch + reset. |
| `PairTable.tsx` | Presentational table used by the Pairs tab. |
| `RawFilterBar.tsx` | `resolution_state` / protocol / kind chip row; selections round-trip through the URL. |
| `DetailCostPreview.tsx` | Confirmation dialog for every Detail scope (`all`, `good_candidates`, `pair`) with the expected `pairs_enqueued` count. |
| `OverflowMenu.tsx` | Page-level menu that launches **Detail: all** / **Detail: good candidates** / **Re-evaluate** via the cost-preview dialog. |

## DrilldownDialog tree

`DrilldownDialog` is a centered modal that hosts:
- A sticky filter toolbar (`CandidatePairFilters`) with four numeric
  filter inputs and a qualifies-only toggle.
- A virtualized, sortable table (`CandidatePairTable`) of per-pair
  scoring rows fetched via `useCandidatePairDetails` (cursor-paginated).
- An inline `MtrPanel` rendered below the table when the operator
  clicks an MTR icon button on a row.

The dialog reads candidate aggregates and active guardrails from the
parent's `useEvaluation` cache; the per-pair rows come from the
paginated endpoint via the dedicated hook.

## Lazy mounting

Radix `TabsContent` renders every panel into the DOM by default. The tab shell
therefore conditionally mounts only the active sub-tab's component, so expensive
per-tab queries (measurements, evaluation, pairs) fire lazily on the tab the
operator actually opens.

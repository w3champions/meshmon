# Results tabs

Per-tab sub-components mounted inside `/campaigns/:id`. The tab shell lives in
`src/pages/CampaignDetail.tsx`; URL param `?tab=` drives the active tab.

## Tabs

| Component | Tab | Role |
|---|---|---|
| `CandidatesTab.tsx` | `candidates` (default) | KPI strip + ranked candidate list + drilldown drawer. |
| `PairsTab.tsx` | `pairs` | One row per baseline pair with row-level force + detail actions. |
| `RawTab.tsx` | `raw` | Every measurement attributed to the campaign, virtualised, with filter chips. |
| `SettingsTab.tsx` | `settings` | Evaluator knobs form with a **Re-evaluate** action. Gated on `completed` / `evaluated`. |

## Supporting components

| File | Role |
|---|---|
| `CandidateTable.tsx` | Sortable table of `EvaluationCandidateDto` rows used by the Candidates tab. |
| `CandidatesTabParts.tsx` | Shared presentational helpers (KPI pills, empty / loading / error states). |
| `DrilldownDrawer.tsx` | Per-candidate side sheet with direct-vs-transit pair details and inline `RouteTopology` MTR rendering. |
| `PairTable.tsx` | Presentational table used by the Pairs tab. |
| `RawFilterBar.tsx` | `resolution_state` / protocol / kind chip row; selections round-trip through the URL. |
| `DetailCostPreview.tsx` | Confirmation dialog for every Detail scope (`all`, `good_candidates`, `pair`) with the expected `pairs_enqueued` count. |
| `OverflowMenu.tsx` | Page-level menu that launches **Detail: all** / **Detail: good candidates** / **Re-evaluate** via the cost-preview dialog. |

## Lazy mounting

Radix `TabsContent` renders every panel into the DOM by default. The tab shell
therefore conditionally mounts only the active sub-tab's component, so expensive
per-tab queries (measurements, evaluation, pairs) fire lazily on the tab the
operator actually opens.

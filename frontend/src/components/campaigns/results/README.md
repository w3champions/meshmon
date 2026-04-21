# Results tabs

Per-tab sub-components mounted inside `/campaigns/:id`. The tab shell lives in
`src/pages/CampaignDetail.tsx`; URL param `?tab=` drives the active tab.

| Component | Tab |
|-----------|-----|
| `CandidatesTab.tsx` | `candidates` (default) |
| `PairsTab.tsx` | `pairs` |
| `RawTab.tsx` | `raw` |
| `SettingsTab.tsx` | `settings` |

Shared pieces (landing in later tasks): `CandidateTable.tsx`,
`DrilldownDrawer.tsx`, `PairTable.tsx`, `DetailCostPreview.tsx`,
`OverflowMenu.tsx`, `RawFilterBar.tsx`.

Radix `TabsContent` renders every panel into the DOM by default. The tab shell
therefore conditionally mounts only the active sub-tab's component, so expensive
per-tab queries (measurements, evaluation, pairs) fire lazily on the tab the
operator actually opens.

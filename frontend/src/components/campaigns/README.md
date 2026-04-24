# `components/campaigns`

React components that compose the `/campaigns*` surface. Every page
mounts `useCampaignStream` once so the SSE fan-out invalidates the
shared TanStack Query cache for every sibling view.

## Component tree

```
CampaignComposer (page at /campaigns/new)
  ├── SourcePanel         — FilterRail + virtual table over /api/agents
  ├── DestinationPanel    — FilterRail + virtual table over /api/catalogue + paste staging
  ├── KnobPanel           — protocol, probes, evaluation mode, force-measurement
  ├── SizePreview         — pre-submit approx + post-submit exact (total / reusable / fresh)
  └── StartConfirmDialog  — fresh-count threshold gate before POST /start

CampaignDetail (page at /campaigns/$id)
  ├── EditMetadataSheet    — title / notes / evaluator knobs PATCH
  ├── Clone button         — terminal-state re-run: seeds composer-seed store then navigates to /campaigns/new
  └── DeleteCampaignDialog — idempotent DELETE with confirm

Campaigns (page at /campaigns)
  └── CampaignRowActions   — per-row Start / Stop / Restart / Clone / Delete menu
```

Every panel is self-contained — the parent page owns selection state,
filter state, and the knob draft. The composer only POSTs to
`/api/campaigns` once **Start** fires; until then nothing leaves the
browser.

## Selection invariant — snapshot-at-click, additive merge

Both panels expose a single **Add all** action. It captures the full
filter match at click time — a later filter change does not mutate the
committed selection.

`SourcePanel` reads the complete agent list from `/api/agents`
client-side and adds every row that matches the rail. `DestinationPanel`
walks the `/api/catalogue` cursor chain at 500 rows per page,
streaming progress inline next to the table, then merges every
returned IP into the existing selection. Prior manual row-clicks or a
narrower previous walk survive the merge, so operators can layer
multiple filtered "Add all" passes to build up a selection.

**Remove all** is the only action that clears state. There is no "Add
matching" sibling — `Add all` already commits every filter match.

## Cache keys and invalidation

All keys are defined in `frontend/src/api/hooks/campaigns.ts`.

| Key | Consumers | Invalidated by |
|---|---|---|
| `CAMPAIGNS_LIST_KEY` (`["campaigns","list"]`) | `/campaigns` list page | create, patch, start, stop, edit, delete, `stream:state_changed`, `stream:lag` |
| `campaignKey(id)` (`["campaigns","entry",id]`) | `/campaigns/$id` detail page | patch, start, stop, edit, force-pair, `stream:state_changed`, `stream:pair_settled` |
| `campaignPairsKey(id)` (`["campaigns","entry",id,"pairs"]`) | Detail pairs list | edit, force-pair, `stream:pair_settled` |
| `campaignPreviewKey(id)` (`["campaigns","preview",id]`) | `SizePreview` phase 2 (post-submit exact count) | start, stop, edit, force-pair, `stream:state_changed`, `stream:pair_settled` |

`campaignPairsKey` is nested under `campaignKey` (both share the
`["campaigns","entry",id,...]` prefix). TanStack Query matches by
prefix, so invalidating `campaignKey(id)` on `state_changed` also
sweeps the pairs cache — that's intentional: pairs depend on the
entry and any state transition can reshape them.

`useCampaignStream` subscribes once per session. `lag` frames (emitted
when the subscriber falls behind the broker's 512-slot buffer) trigger
a full `CAMPAIGNS_LIST_KEY` + `CAMPAIGN_PREVIEW_KEY` sweep so the UI
re-syncs rather than drifting on a stale view.

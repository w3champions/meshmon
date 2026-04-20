import { Link, useNavigate, useSearch } from "@tanstack/react-router";
import { ArrowDown, ArrowUp } from "lucide-react";
import { useCallback, useEffect, useMemo, useState } from "react";
import { useCampaignStream } from "@/api/hooks/campaign-stream";
import {
  type Campaign,
  type CampaignListQuery,
  type CampaignState,
  useCampaignsList,
  useDeleteCampaign,
  useEditCampaign,
  useStartCampaign,
  useStopCampaign,
} from "@/api/hooks/campaigns";
import { CampaignRowActions } from "@/components/campaigns/CampaignRowActions";
import { DeleteCampaignDialog } from "@/components/campaigns/DeleteCampaignDialog";
import { EditMetadataSheet } from "@/components/campaigns/EditMetadataSheet";
import { EditPairsSheet } from "@/components/campaigns/EditPairsSheet";
import { Badge } from "@/components/ui/badge";
import { Button } from "@/components/ui/button";
import { Card } from "@/components/ui/card";
import { Input } from "@/components/ui/input";
import { Label } from "@/components/ui/label";
import {
  Select,
  SelectContent,
  SelectItem,
  SelectTrigger,
  SelectValue,
} from "@/components/ui/select";
import { Skeleton } from "@/components/ui/skeleton";
import {
  Table,
  TableBody,
  TableCell,
  TableHead,
  TableHeader,
  TableRow,
} from "@/components/ui/table";
import { isIllegalStateTransition, stateBadgeVariant } from "@/lib/campaign";
import { useToastStore } from "@/stores/toast";

// ---------------------------------------------------------------------------
// URL-backed search shape (see router.campaignsSearchSchema)
// ---------------------------------------------------------------------------

type SortColumn = "title" | "created_at" | "started_at" | "state";
type SortDir = "asc" | "desc";

interface CampaignsSearch {
  q?: string;
  state?: CampaignState;
  created_by?: string;
  sort?: SortColumn;
  dir?: SortDir;
}

type StateFilterValue = CampaignState | "all";

const DEBOUNCE_MS = 300;
const DEFAULT_SORT: SortColumn = "created_at";
const DEFAULT_DIR: SortDir = "desc";

// ---------------------------------------------------------------------------
// Client-side sort helper. The list endpoint ignores sort parameters so the
// page sorts in-memory.
// ---------------------------------------------------------------------------

function compareBy(a: Campaign, b: Campaign, col: SortColumn): number {
  switch (col) {
    case "title":
      return a.title.localeCompare(b.title);
    case "state":
      return a.state.localeCompare(b.state);
    case "created_at":
      return a.created_at.localeCompare(b.created_at);
    case "started_at": {
      // Nulls sort as "oldest" — draft campaigns that never started stay at
      // the bottom on `desc` and the top on `asc`.
      const av = a.started_at ?? "";
      const bv = b.started_at ?? "";
      return av.localeCompare(bv);
    }
  }
}

function sortByColumn(rows: Campaign[], col: SortColumn, dir: SortDir): Campaign[] {
  const copy = [...rows];
  copy.sort((a, b) => {
    const result = compareBy(a, b, col);
    return dir === "asc" ? result : -result;
  });
  return copy;
}

// ---------------------------------------------------------------------------
// Sortable column header
// ---------------------------------------------------------------------------

interface SortHeaderProps {
  col: SortColumn;
  label: string;
  activeCol: SortColumn;
  activeDir: SortDir;
  onToggle: (col: SortColumn) => void;
}

function SortHeader({ col, label, activeCol, activeDir, onToggle }: SortHeaderProps) {
  const isActive = activeCol === col;
  return (
    <Button
      variant="ghost"
      size="sm"
      className="-ml-3 h-8 px-2"
      onClick={() => onToggle(col)}
      aria-label={`Sort by ${label}`}
    >
      {label}
      {isActive ? (
        activeDir === "asc" ? (
          <ArrowUp aria-hidden="true" />
        ) : (
          <ArrowDown aria-hidden="true" />
        )
      ) : null}
    </Button>
  );
}

/**
 * WAI-ARIA: `aria-sort` is only honoured on elements with the
 * `columnheader` / `rowheader` role (i.e., `<th>`). Callers attach the
 * computed sort state here, not on the inner `<Button>` that drives the
 * toggle — otherwise screen readers ignore it.
 */
function sortableHeaderAriaSort(
  isActive: boolean,
  dir: SortDir,
): "ascending" | "descending" | "none" {
  if (!isActive) return "none";
  return dir === "asc" ? "ascending" : "descending";
}

// ---------------------------------------------------------------------------
// Page
// ---------------------------------------------------------------------------

export default function Campaigns() {
  // Mount the SSE stream once. The stream invalidates the list cache on every
  // lifecycle event, so the 15s polling in `useCampaignsList` is a safety
  // net rather than the primary freshness mechanism.
  useCampaignStream();

  const rawSearch = useSearch({ strict: false }) as CampaignsSearch;
  const navigate = useNavigate();

  const sort: SortColumn = rawSearch.sort ?? DEFAULT_SORT;
  const dir: SortDir = rawSearch.dir ?? DEFAULT_DIR;

  // Local state for the debounced text inputs. The URL is authoritative — we
  // debounce by running a timer that pushes the input value into the URL.
  const [searchDraft, setSearchDraft] = useState<string>(rawSearch.q ?? "");
  const [createdByDraft, setCreatedByDraft] = useState<string>(rawSearch.created_by ?? "");

  // Keep local inputs in sync when the URL changes externally (back/forward,
  // initial navigation, filter-clear button).
  useEffect(() => {
    setSearchDraft(rawSearch.q ?? "");
  }, [rawSearch.q]);
  useEffect(() => {
    setCreatedByDraft(rawSearch.created_by ?? "");
  }, [rawSearch.created_by]);

  const updateSearch = useCallback(
    (patch: Partial<CampaignsSearch>): void => {
      void (navigate as (opts: { search: unknown; replace: boolean }) => void)({
        search: { ...rawSearch, ...patch } as CampaignsSearch,
        replace: true,
      });
    },
    [navigate, rawSearch],
  );

  // Debounce the draft `q` → URL.
  useEffect(() => {
    const next = searchDraft === "" ? undefined : searchDraft;
    if (next === rawSearch.q) return;
    const handle = setTimeout(() => {
      updateSearch({ q: next });
    }, DEBOUNCE_MS);
    return () => clearTimeout(handle);
  }, [searchDraft, rawSearch.q, updateSearch]);

  // Debounce the draft `created_by` → URL.
  useEffect(() => {
    const next = createdByDraft === "" ? undefined : createdByDraft;
    if (next === rawSearch.created_by) return;
    const handle = setTimeout(() => {
      updateSearch({ created_by: next });
    }, DEBOUNCE_MS);
    return () => clearTimeout(handle);
  }, [createdByDraft, rawSearch.created_by, updateSearch]);

  const listQuery: CampaignListQuery = useMemo(() => {
    const q: CampaignListQuery = {};
    if (rawSearch.q) q.q = rawSearch.q;
    if (rawSearch.state) q.state = rawSearch.state;
    if (rawSearch.created_by) q.created_by = rawSearch.created_by;
    return q;
  }, [rawSearch.q, rawSearch.state, rawSearch.created_by]);

  const campaignsQuery = useCampaignsList(listQuery);

  const rows = useMemo(
    () => sortByColumn(campaignsQuery.data ?? [], sort, dir),
    [campaignsQuery.data, sort, dir],
  );

  const hasActiveFilters =
    Boolean(rawSearch.q) || Boolean(rawSearch.state) || Boolean(rawSearch.created_by);

  // -------------------------------------------------------------------------
  // Action handlers
  // -------------------------------------------------------------------------

  // TanStack Query v5 returns a new result object every render, so listing
  // the whole mutation in a `useCallback` dep array defeats memoization.
  // Destructure `.mutate` — it's a stable reference per mutation. The
  // delete path also reads `isPending` to disable the dialog's confirm
  // button, so we keep the full result object for that site.
  const { mutate: startCampaign } = useStartCampaign();
  const { mutate: stopCampaign } = useStopCampaign();
  // Restart uses the edit endpoint with an empty body — the server transitions
  // {completed, stopped, evaluated} → running without touching pair state.
  // Operators who want to re-run every pair use "Edit pairs" with force
  // measurement, or (post-T49) a dedicated Force-remeasure action.
  const { mutate: editCampaign } = useEditCampaign();
  const deleteMutation = useDeleteCampaign();
  const { mutate: deleteCampaign } = deleteMutation;

  const [editMetadataTarget, setEditMetadataTarget] = useState<Campaign | null>(null);
  const [editPairsTarget, setEditPairsTarget] = useState<Campaign | null>(null);
  const [deleteTarget, setDeleteTarget] = useState<Campaign | null>(null);

  const handleStart = useCallback(
    (id: string): void => {
      startCampaign(id, {
        onError: (err) => {
          const { pushToast } = useToastStore.getState();
          if (isIllegalStateTransition(err)) {
            pushToast({
              kind: "error",
              message: "Can't start — this campaign already advanced.",
            });
            return;
          }
          pushToast({
            kind: "error",
            message: `Start failed: ${err.message}`,
          });
        },
      });
    },
    [startCampaign],
  );
  const handleStop = useCallback(
    (id: string): void => {
      stopCampaign(id, {
        onError: (err) => {
          const { pushToast } = useToastStore.getState();
          if (isIllegalStateTransition(err)) {
            pushToast({
              kind: "error",
              message: "Can't stop — this campaign has already finished.",
            });
            return;
          }
          pushToast({
            kind: "error",
            message: `Stop failed: ${err.message}`,
          });
        },
      });
    },
    [stopCampaign],
  );
  const handleRestart = useCallback(
    (id: string): void => {
      editCampaign(
        { id, body: {} },
        {
          onError: (err) => {
            const { pushToast } = useToastStore.getState();
            if (isIllegalStateTransition(err)) {
              pushToast({
                kind: "error",
                message: "Can't restart — campaign advanced before the request landed.",
              });
              return;
            }
            pushToast({
              kind: "error",
              message: `Restart failed: ${err.message}`,
            });
          },
        },
      );
    },
    [editCampaign],
  );
  const handleEditMetadata = useCallback((campaign: Campaign): void => {
    setEditMetadataTarget(campaign);
  }, []);
  const handleEditPairs = useCallback((campaign: Campaign): void => {
    setEditPairsTarget(campaign);
  }, []);
  const handleDelete = useCallback((campaign: Campaign): void => {
    setDeleteTarget(campaign);
  }, []);
  const handleConfirmDelete = useCallback(
    (id: string): void => {
      deleteCampaign(id, {
        onSuccess: () => setDeleteTarget(null),
        onError: (err) => {
          // Note: the backend's delete handler (`repo.rs`) is not
          // lifecycle-gated, so we don't branch on 409 here — only the
          // generic fallback. Close the dialog either way so the operator
          // isn't left with a stuck-open confirm while the toast fires.
          setDeleteTarget(null);
          const { pushToast } = useToastStore.getState();
          pushToast({
            kind: "error",
            message: `Delete failed: ${err.message}`,
          });
        },
      });
    },
    [deleteCampaign],
  );

  // -------------------------------------------------------------------------
  // Sort handler. Clicking the active column toggles direction; clicking a
  // different column resets to `asc`.
  // -------------------------------------------------------------------------

  const handleToggleSort = useCallback(
    (col: SortColumn): void => {
      if (sort === col) {
        updateSearch({ sort: col, dir: dir === "asc" ? "desc" : "asc" });
      } else {
        updateSearch({ sort: col, dir: "asc" });
      }
    },
    [sort, dir, updateSearch],
  );

  const handleClearFilters = useCallback((): void => {
    updateSearch({ q: undefined, state: undefined, created_by: undefined });
  }, [updateSearch]);

  // -------------------------------------------------------------------------
  // Render
  // -------------------------------------------------------------------------

  return (
    <div className="flex flex-col gap-4">
      <header className="flex flex-wrap items-end gap-3">
        <div className="flex flex-col gap-1">
          <Label htmlFor="campaigns-search">Search</Label>
          <Input
            id="campaigns-search"
            aria-label="Search title or notes"
            placeholder="Search title or notes…"
            className="w-64"
            value={searchDraft}
            onChange={(e) => setSearchDraft(e.target.value)}
          />
        </div>
        <div className="flex flex-col gap-1">
          <Label htmlFor="campaigns-state">State</Label>
          <Select
            value={(rawSearch.state ?? "all") as StateFilterValue}
            onValueChange={(v: StateFilterValue) =>
              updateSearch({ state: v === "all" ? undefined : v })
            }
          >
            <SelectTrigger id="campaigns-state" aria-label="State" className="w-40">
              <SelectValue />
            </SelectTrigger>
            <SelectContent>
              <SelectItem value="all">All</SelectItem>
              <SelectItem value="draft">Draft</SelectItem>
              <SelectItem value="running">Running</SelectItem>
              <SelectItem value="completed">Completed</SelectItem>
              <SelectItem value="evaluated">Evaluated</SelectItem>
              <SelectItem value="stopped">Stopped</SelectItem>
            </SelectContent>
          </Select>
        </div>
        <div className="flex flex-col gap-1">
          <Label htmlFor="campaigns-created-by">Created by</Label>
          <Input
            id="campaigns-created-by"
            aria-label="Created by"
            placeholder="username"
            className="w-40"
            value={createdByDraft}
            onChange={(e) => setCreatedByDraft(e.target.value)}
          />
        </div>
        <div className="ml-auto">
          <Button asChild>
            <Link to="/campaigns/new">Create campaign</Link>
          </Button>
        </div>
      </header>

      {campaignsQuery.isLoading ? (
        <div className="flex flex-col gap-2" data-testid="campaigns-loading">
          <Skeleton className="h-10 w-full" />
          <Skeleton className="h-10 w-full" />
          <Skeleton className="h-10 w-full" />
        </div>
      ) : campaignsQuery.isError ? (
        <Card className="flex flex-col items-start gap-3 p-6">
          <p role="alert" className="text-sm text-destructive">
            Failed to load campaigns.
          </p>
          <Button onClick={() => campaignsQuery.refetch()}>Retry</Button>
        </Card>
      ) : rows.length === 0 ? (
        hasActiveFilters ? (
          <Card className="flex flex-col items-center gap-3 p-8 text-center">
            <p className="text-sm">No campaigns match the filters.</p>
            <Button variant="outline" onClick={handleClearFilters}>
              Clear filters
            </Button>
          </Card>
        ) : (
          <Card className="flex flex-col items-center gap-3 p-8 text-center">
            <p className="text-sm">No campaigns yet.</p>
            <Button asChild>
              <Link to="/campaigns/new">Create campaign</Link>
            </Button>
          </Card>
        )
      ) : (
        // Pair-count column is intentionally omitted: `GET /api/campaigns` returns
        // `pair_counts: []` on every row to avoid an N+1 COUNT fan-out. The
        // detail page is the canonical surface for pair-state roll-ups.
        <Table>
          <TableHeader>
            <TableRow>
              <TableHead scope="col" aria-sort={sortableHeaderAriaSort(sort === "title", dir)}>
                <SortHeader
                  col="title"
                  label="Title"
                  activeCol={sort}
                  activeDir={dir}
                  onToggle={handleToggleSort}
                />
              </TableHead>
              <TableHead scope="col" aria-sort={sortableHeaderAriaSort(sort === "state", dir)}>
                <SortHeader
                  col="state"
                  label="State"
                  activeCol={sort}
                  activeDir={dir}
                  onToggle={handleToggleSort}
                />
              </TableHead>
              <TableHead scope="col">Protocol</TableHead>
              <TableHead scope="col" aria-sort={sortableHeaderAriaSort(sort === "created_at", dir)}>
                <SortHeader
                  col="created_at"
                  label="Created"
                  activeCol={sort}
                  activeDir={dir}
                  onToggle={handleToggleSort}
                />
              </TableHead>
              <TableHead scope="col">Created by</TableHead>
              <TableHead scope="col" className="w-16 text-right">
                Actions
              </TableHead>
            </TableRow>
          </TableHeader>
          <TableBody>
            {rows.map((campaign) => (
              <TableRow
                key={campaign.id}
                data-testid={`campaign-row-${campaign.id}`}
                // Click anywhere on the row to open the detail page. The
                // title `<Link>` still carries the semantic href so
                // right-click / middle-click / screen-reader users retain a
                // first-class navigation target; the row-level handler is a
                // convenience, not the sole path.
                role="button"
                tabIndex={0}
                onClick={(event) => {
                  // Ignore clicks that originated inside an interactive
                  // child (links, buttons, the actions menu). Without this
                  // the row-level navigation would race with (e.g.) the
                  // Delete dropdown item and navigate mid-mutation.
                  if ((event.target as HTMLElement).closest("a,button,[role='menuitem']")) {
                    return;
                  }
                  void navigate({ to: "/campaigns/$id", params: { id: campaign.id } });
                }}
                onKeyDown={(event) => {
                  if (event.key !== "Enter" && event.key !== " ") return;
                  if ((event.target as HTMLElement).closest("a,button,[role='menuitem']")) {
                    return;
                  }
                  event.preventDefault();
                  void navigate({ to: "/campaigns/$id", params: { id: campaign.id } });
                }}
                className="cursor-pointer"
              >
                <TableCell className="font-medium">
                  <Link
                    to="/campaigns/$id"
                    params={{ id: campaign.id }}
                    className="hover:underline"
                  >
                    {campaign.title}
                  </Link>
                </TableCell>
                <TableCell>
                  <Badge variant={stateBadgeVariant(campaign.state)}>{campaign.state}</Badge>
                </TableCell>
                <TableCell className="uppercase">{campaign.protocol}</TableCell>
                <TableCell>{campaign.created_at}</TableCell>
                <TableCell>{campaign.created_by ?? "—"}</TableCell>
                <TableCell className="text-right">
                  <CampaignRowActions
                    campaign={campaign}
                    onStart={handleStart}
                    onStop={handleStop}
                    onRestart={handleRestart}
                    onEditMetadata={handleEditMetadata}
                    onEditPairs={handleEditPairs}
                    onDelete={handleDelete}
                  />
                </TableCell>
              </TableRow>
            ))}
          </TableBody>
        </Table>
      )}

      <EditMetadataSheet
        campaign={editMetadataTarget}
        open={editMetadataTarget !== null}
        onOpenChange={(next) => {
          if (!next) setEditMetadataTarget(null);
        }}
      />
      <EditPairsSheet
        campaign={editPairsTarget}
        open={editPairsTarget !== null}
        onOpenChange={(next) => {
          if (!next) setEditPairsTarget(null);
        }}
      />
      <DeleteCampaignDialog
        campaign={deleteTarget}
        open={deleteTarget !== null}
        onOpenChange={(next) => {
          if (!next) setDeleteTarget(null);
        }}
        onConfirm={handleConfirmDelete}
        isPending={deleteMutation.isPending}
      />
    </div>
  );
}

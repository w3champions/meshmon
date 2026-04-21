import { MoreHorizontal } from "lucide-react";
import type { Campaign } from "@/api/hooks/campaigns";
import { Button } from "@/components/ui/button";
import {
  DropdownMenu,
  DropdownMenuContent,
  DropdownMenuItem,
  DropdownMenuTrigger,
} from "@/components/ui/dropdown-menu";

interface CampaignRowActionsProps {
  campaign: Campaign;
  onStart: (id: string) => void;
  onStop: (id: string) => void;
  onRestart: (id: string) => void;
  onEditMetadata: (campaign: Campaign) => void;
  onClone: (campaign: Campaign) => void;
  onDelete: (campaign: Campaign) => void;
}

/**
 * Per-row action menu. State-gated to surface only the transitions allowed by
 * the backend lifecycle machine:
 *
 * - `draft`: Start, Edit metadata, Delete
 * - `running`: Stop, Edit metadata
 * - `completed` / `stopped` / `evaluated`: Restart, Edit metadata, Clone, Delete
 *
 * "Restart" calls `POST /api/campaigns/:id/edit` with an empty body — the
 * server re-enters `running` without touching pair state. Re-running the
 * campaign with tweaked knobs uses Clone, which lives on the detail page
 * (the seed needs the campaign's pair list, which this list view does
 * not load per-row).
 */
export function CampaignRowActions({
  campaign,
  onStart,
  onStop,
  onRestart,
  onEditMetadata,
  onClone,
  onDelete,
}: CampaignRowActionsProps) {
  const { state } = campaign;
  const isTerminal = state === "completed" || state === "stopped" || state === "evaluated";
  return (
    <DropdownMenu>
      <DropdownMenuTrigger asChild>
        <Button variant="ghost" size="icon" aria-label={`Actions for ${campaign.title}`}>
          <MoreHorizontal />
        </Button>
      </DropdownMenuTrigger>
      <DropdownMenuContent align="end">
        {state === "draft" ? (
          <DropdownMenuItem onClick={() => onStart(campaign.id)}>Start</DropdownMenuItem>
        ) : null}
        {state === "running" ? (
          <DropdownMenuItem onClick={() => onStop(campaign.id)}>Stop</DropdownMenuItem>
        ) : null}
        {isTerminal ? (
          <DropdownMenuItem onClick={() => onRestart(campaign.id)}>Restart</DropdownMenuItem>
        ) : null}
        <DropdownMenuItem onClick={() => onEditMetadata(campaign)}>Edit metadata</DropdownMenuItem>
        {isTerminal ? (
          <DropdownMenuItem onClick={() => onClone(campaign)}>Clone</DropdownMenuItem>
        ) : null}
        {state === "draft" || isTerminal ? (
          <DropdownMenuItem
            className="text-destructive focus:text-destructive"
            onClick={() => onDelete(campaign)}
          >
            Delete
          </DropdownMenuItem>
        ) : null}
      </DropdownMenuContent>
    </DropdownMenu>
  );
}

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
  onEditMetadata: (campaign: Campaign) => void;
  onEditPairs: (campaign: Campaign) => void;
  onDelete: (campaign: Campaign) => void;
}

/**
 * Per-row action menu. State-gated to surface only the transitions allowed by
 * the backend lifecycle machine:
 *
 * - `draft`: Start, Edit metadata, Delete
 * - `running`: Stop, Edit metadata
 * - `completed` / `stopped` / `evaluated`: Edit metadata, Edit pairs, Delete
 */
export function CampaignRowActions({
  campaign,
  onStart,
  onStop,
  onEditMetadata,
  onEditPairs,
  onDelete,
}: CampaignRowActionsProps) {
  const { state } = campaign;
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
        <DropdownMenuItem onClick={() => onEditMetadata(campaign)}>Edit metadata</DropdownMenuItem>
        {state === "completed" || state === "stopped" || state === "evaluated" ? (
          <DropdownMenuItem onClick={() => onEditPairs(campaign)}>Edit pairs</DropdownMenuItem>
        ) : null}
        {state === "draft" ||
        state === "completed" ||
        state === "stopped" ||
        state === "evaluated" ? (
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

import type { Campaign } from "@/api/hooks/campaigns";
import { Button } from "@/components/ui/button";
import {
  Dialog,
  DialogContent,
  DialogDescription,
  DialogFooter,
  DialogHeader,
  DialogTitle,
} from "@/components/ui/dialog";

interface DeleteCampaignDialogProps {
  campaign: Campaign | null;
  open: boolean;
  onOpenChange: (open: boolean) => void;
  onConfirm: (id: string) => void;
  isPending?: boolean;
}

/**
 * Destructive confirmation dialog for `DELETE /api/campaigns/{id}`. Mirrors
 * the shadcn `AlertDialog` shape using the existing `Dialog` primitive, since
 * this codebase does not ship `alert-dialog.tsx`.
 */
export function DeleteCampaignDialog({
  campaign,
  open,
  onOpenChange,
  onConfirm,
  isPending,
}: DeleteCampaignDialogProps) {
  if (!campaign) return null;
  return (
    <Dialog open={open} onOpenChange={onOpenChange}>
      <DialogContent role="alertdialog">
        <DialogHeader>
          <DialogTitle>Delete campaign</DialogTitle>
          <DialogDescription>
            Delete campaign &quot;{campaign.title}&quot;? This cannot be undone.
          </DialogDescription>
        </DialogHeader>
        <DialogFooter>
          <Button variant="outline" onClick={() => onOpenChange(false)}>
            Cancel
          </Button>
          <Button variant="destructive" onClick={() => onConfirm(campaign.id)} disabled={isPending}>
            {isPending ? "Deleting…" : "Delete"}
          </Button>
        </DialogFooter>
      </DialogContent>
    </Dialog>
  );
}

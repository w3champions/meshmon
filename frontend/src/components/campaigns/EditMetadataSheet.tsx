import { useEffect, useState } from "react";
import {
  type Campaign,
  type EvaluationMode,
  type PatchCampaignBody,
  usePatchCampaign,
} from "@/api/hooks/campaigns";
import { Button } from "@/components/ui/button";
import { Input } from "@/components/ui/input";
import { Label } from "@/components/ui/label";
import {
  Select,
  SelectContent,
  SelectItem,
  SelectTrigger,
  SelectValue,
} from "@/components/ui/select";
import {
  Sheet,
  SheetContent,
  SheetDescription,
  SheetFooter,
  SheetHeader,
  SheetTitle,
} from "@/components/ui/sheet";
import { isIllegalStateTransition } from "@/lib/campaign";
import { useToastStore } from "@/stores/toast";

interface EditMetadataSheetProps {
  campaign: Campaign | null;
  open: boolean;
  onOpenChange: (open: boolean) => void;
}

/**
 * Side panel for editing a campaign's metadata (title, notes, loss threshold,
 * stddev weight, evaluation mode). Each field is optional — absent fields
 * leave the server-side column untouched.
 */
export function EditMetadataSheet({ campaign, open, onOpenChange }: EditMetadataSheetProps) {
  const patchMutation = usePatchCampaign();

  const [title, setTitle] = useState<string>("");
  const [notes, setNotes] = useState<string>("");
  const [lossThresholdPct, setLossThresholdPct] = useState<string>("");
  const [stddevWeight, setStddevWeight] = useState<string>("");
  const [evaluationMode, setEvaluationMode] = useState<EvaluationMode | "unchanged">("unchanged");
  const [error, setError] = useState<string | null>(null);

  useEffect(() => {
    if (campaign && open) {
      setTitle(campaign.title);
      setNotes(campaign.notes);
      setLossThresholdPct(String(campaign.loss_threshold_pct));
      setStddevWeight(String(campaign.stddev_weight));
      setEvaluationMode(campaign.evaluation_mode);
      setError(null);
    }
  }, [campaign, open]);

  // Reset stale mutation state when the sheet is reopened. Without this, a
  // previous in-flight save that hadn't resolved before close would keep
  // `isPending=true`, leaving the Save button disabled on the next open.
  // `patchMutation.reset` is a stable reference from TanStack Query.
  const resetPatchMutation = patchMutation.reset;
  useEffect(() => {
    if (open) resetPatchMutation();
  }, [open, resetPatchMutation]);

  if (!campaign) return null;

  const handleSave = (): void => {
    setError(null);
    const body: PatchCampaignBody = {};
    const trimmedTitle = title.trim();
    if (trimmedTitle !== campaign.title) {
      if (trimmedTitle.length === 0) {
        setError("Title cannot be blank.");
        return;
      }
      body.title = trimmedTitle;
    }
    if (notes !== campaign.notes) {
      body.notes = notes;
    }
    if (lossThresholdPct !== String(campaign.loss_threshold_pct)) {
      const parsed = Number.parseFloat(lossThresholdPct);
      if (!Number.isFinite(parsed)) {
        setError("Loss threshold must be a number.");
        return;
      }
      body.loss_threshold_pct = parsed;
    }
    if (stddevWeight !== String(campaign.stddev_weight)) {
      const parsed = Number.parseFloat(stddevWeight);
      if (!Number.isFinite(parsed)) {
        setError("Stddev weight must be a number.");
        return;
      }
      body.stddev_weight = parsed;
    }
    if (evaluationMode !== "unchanged" && evaluationMode !== campaign.evaluation_mode) {
      body.evaluation_mode = evaluationMode;
    }

    if (Object.keys(body).length === 0) {
      onOpenChange(false);
      return;
    }

    patchMutation.mutate(
      { id: campaign.id, body },
      {
        onSuccess: () => onOpenChange(false),
        onError: (err) => {
          const { pushToast } = useToastStore.getState();
          if (isIllegalStateTransition(err)) {
            pushToast({
              kind: "error",
              message: "This campaign advanced before your edit landed.",
            });
            return;
          }
          pushToast({
            kind: "error",
            message: `Save failed: ${err.message}`,
          });
        },
      },
    );
  };

  return (
    <Sheet open={open} onOpenChange={onOpenChange}>
      <SheetContent className="flex flex-col gap-4 sm:max-w-md">
        <SheetHeader>
          <SheetTitle>Edit metadata</SheetTitle>
          <SheetDescription>
            Update the campaign's display and evaluator settings. Lifecycle state is untouched.
          </SheetDescription>
        </SheetHeader>
        <div className="flex flex-col gap-3">
          <div className="flex flex-col gap-1">
            <Label htmlFor="edit-title">Title</Label>
            <Input id="edit-title" value={title} onChange={(e) => setTitle(e.target.value)} />
          </div>
          <div className="flex flex-col gap-1">
            <Label htmlFor="edit-notes">Notes</Label>
            <textarea
              id="edit-notes"
              className="min-h-[4rem] w-full rounded-md border border-input bg-background px-3 py-2 text-sm"
              value={notes}
              onChange={(e) => setNotes(e.target.value)}
            />
          </div>
          <div className="flex flex-col gap-1">
            <Label htmlFor="edit-loss">Loss threshold (%)</Label>
            <Input
              id="edit-loss"
              type="number"
              step="0.1"
              value={lossThresholdPct}
              onChange={(e) => setLossThresholdPct(e.target.value)}
            />
          </div>
          <div className="flex flex-col gap-1">
            <Label htmlFor="edit-stddev">Stddev weight</Label>
            <Input
              id="edit-stddev"
              type="number"
              step="0.1"
              value={stddevWeight}
              onChange={(e) => setStddevWeight(e.target.value)}
            />
          </div>
          <div className="flex flex-col gap-1">
            <Label htmlFor="edit-eval">Evaluation mode</Label>
            <Select
              value={evaluationMode}
              onValueChange={(v) => setEvaluationMode(v as EvaluationMode | "unchanged")}
            >
              <SelectTrigger id="edit-eval" aria-label="Evaluation mode">
                <SelectValue />
              </SelectTrigger>
              <SelectContent>
                <SelectItem value="unchanged">(unchanged)</SelectItem>
                <SelectItem value="diversity">diversity</SelectItem>
                <SelectItem value="optimization">optimization</SelectItem>
              </SelectContent>
            </Select>
          </div>
          {error !== null ? (
            <p role="alert" className="text-sm text-destructive">
              {error}
            </p>
          ) : null}
        </div>
        <SheetFooter>
          <Button variant="outline" onClick={() => onOpenChange(false)}>
            Cancel
          </Button>
          <Button onClick={handleSave} disabled={patchMutation.isPending}>
            {patchMutation.isPending ? "Saving…" : "Save"}
          </Button>
        </SheetFooter>
      </SheetContent>
    </Sheet>
  );
}

import { useEffect, useState } from "react";
import { type Campaign, type EditCampaignBody, useEditCampaign } from "@/api/hooks/campaigns";
import { Button } from "@/components/ui/button";
import { Label } from "@/components/ui/label";
import {
  Sheet,
  SheetContent,
  SheetDescription,
  SheetFooter,
  SheetHeader,
  SheetTitle,
} from "@/components/ui/sheet";

interface EditPairsSheetProps {
  campaign: Campaign | null;
  open: boolean;
  onOpenChange: (open: boolean) => void;
}

interface Pair {
  source_agent_id: string;
  destination_ip: string;
}

interface ParseResult {
  pairs: Pair[];
  invalidLines: number[];
}

/**
 * Parse a multi-line `source_agent_id,destination_ip` block. Blank lines are
 * ignored. Lines that don't split into exactly two non-blank tokens are
 * reported back via `invalidLines` (1-indexed) so the caller can surface the
 * offending entry.
 */
function parsePairLines(text: string): ParseResult {
  const pairs: Pair[] = [];
  const invalidLines: number[] = [];
  const lines = text.split("\n");
  for (let i = 0; i < lines.length; i += 1) {
    const raw = lines[i].trim();
    if (raw === "") continue;
    const parts = raw.split(",").map((p) => p.trim());
    if (parts.length !== 2 || parts[0] === "" || parts[1] === "") {
      invalidLines.push(i + 1);
      continue;
    }
    pairs.push({ source_agent_id: parts[0], destination_ip: parts[1] });
  }
  return { pairs, invalidLines };
}

/**
 * Side panel for editing a campaign's pair set. Two textareas feed `add_pairs`
 * and `remove_pairs`; an optional `force_measurement` toggle resets every
 * pair so the whole campaign re-runs. Sends `POST /api/campaigns/{id}/edit`.
 */
export function EditPairsSheet({ campaign, open, onOpenChange }: EditPairsSheetProps) {
  const editMutation = useEditCampaign();
  const [addText, setAddText] = useState<string>("");
  const [removeText, setRemoveText] = useState<string>("");
  const [forceMeasurement, setForceMeasurement] = useState<boolean>(false);
  const [error, setError] = useState<string | null>(null);

  useEffect(() => {
    if (open) {
      setAddText("");
      setRemoveText("");
      setForceMeasurement(false);
      setError(null);
    }
  }, [open]);

  // Reset stale mutation state when the sheet is reopened. Without this, a
  // previous in-flight edit that hadn't resolved before close would keep
  // `isPending=true`, leaving the Save button disabled on the next open.
  // `editMutation.reset` is a stable reference from TanStack Query.
  const resetEditMutation = editMutation.reset;
  useEffect(() => {
    if (open) resetEditMutation();
  }, [open, resetEditMutation]);

  if (!campaign) return null;

  const handleSave = (): void => {
    setError(null);
    const add = parsePairLines(addText);
    const remove = parsePairLines(removeText);
    const invalid = [
      ...add.invalidLines.map((n) => `Add line ${n}`),
      ...remove.invalidLines.map((n) => `Remove line ${n}`),
    ];
    if (invalid.length > 0) {
      setError(
        `Malformed: ${invalid.join(", ")}. Each line must be "source_agent_id,destination_ip".`,
      );
      return;
    }

    const body: EditCampaignBody = {};
    if (add.pairs.length > 0) body.add_pairs = add.pairs;
    if (remove.pairs.length > 0) body.remove_pairs = remove.pairs;
    if (forceMeasurement) body.force_measurement = true;

    if (Object.keys(body).length === 0) {
      onOpenChange(false);
      return;
    }

    editMutation.mutate(
      { id: campaign.id, body },
      {
        onSuccess: () => onOpenChange(false),
      },
    );
  };

  return (
    <Sheet open={open} onOpenChange={onOpenChange}>
      <SheetContent className="flex flex-col gap-4 sm:max-w-lg">
        <SheetHeader>
          <SheetTitle>Edit pairs</SheetTitle>
          <SheetDescription>
            Add or remove `(source_agent, destination_ip)` pairs. The campaign re-enters `running`
            when the server applies the delta.
          </SheetDescription>
        </SheetHeader>
        <div className="flex flex-col gap-3">
          <div className="flex flex-col gap-1">
            <Label htmlFor="edit-add-pairs">
              Add pairs: one `source_agent_id,destination_ip` per line
            </Label>
            <textarea
              id="edit-add-pairs"
              className="min-h-[6rem] w-full rounded-md border border-input bg-background px-3 py-2 font-mono text-xs"
              value={addText}
              onChange={(e) => setAddText(e.target.value)}
              placeholder="agent-1,10.0.0.1"
            />
          </div>
          <div className="flex flex-col gap-1">
            <Label htmlFor="edit-remove-pairs">
              Remove pairs: one `source_agent_id,destination_ip` per line
            </Label>
            <textarea
              id="edit-remove-pairs"
              className="min-h-[6rem] w-full rounded-md border border-input bg-background px-3 py-2 font-mono text-xs"
              value={removeText}
              onChange={(e) => setRemoveText(e.target.value)}
              placeholder="agent-1,10.0.0.2"
            />
          </div>
          <label className="flex items-center gap-2 text-sm">
            <input
              type="checkbox"
              checked={forceMeasurement}
              onChange={(e) => setForceMeasurement(e.target.checked)}
            />
            <span>Force measurement (reset every pair)</span>
          </label>
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
          <Button onClick={handleSave} disabled={editMutation.isPending}>
            {editMutation.isPending ? "Saving…" : "Save"}
          </Button>
        </SheetFooter>
      </SheetContent>
    </Sheet>
  );
}

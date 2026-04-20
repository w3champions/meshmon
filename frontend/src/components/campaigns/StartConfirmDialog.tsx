import { Button } from "@/components/ui/button";
import {
  Dialog,
  DialogContent,
  DialogDescription,
  DialogFooter,
  DialogHeader,
  DialogTitle,
} from "@/components/ui/dialog";

export interface StartConfirmDialogProps {
  open: boolean;
  onOpenChange(open: boolean): void;
  /** Fresh measurements the scheduler is about to dispatch. */
  freshCount: number;
  /** Invoked when the operator confirms. */
  onConfirm(): void;
  /**
   * True while the parent's Start mutation is in flight. Disables both
   * the Start button and dismissal so the dialog can't get out of sync
   * with the mutation.
   */
  isStarting: boolean;
}

/**
 * Confirmation prompt shown when the campaign's fresh-measurement count
 * exceeds `SIZE_WARNING_THRESHOLD`. Presentation-only; all state lives
 * in the composer.
 */
export function StartConfirmDialog({
  open,
  onOpenChange,
  freshCount,
  onConfirm,
  isStarting,
}: StartConfirmDialogProps) {
  return (
    <Dialog
      open={open}
      onOpenChange={(next) => {
        if (!isStarting) onOpenChange(next);
      }}
    >
      <DialogContent>
        <DialogHeader>
          <DialogTitle>Confirm large dispatch</DialogTitle>
          <DialogDescription>
            This will dispatch ~{freshCount} new measurements. Continue?
          </DialogDescription>
        </DialogHeader>
        <DialogFooter>
          <Button
            type="button"
            variant="outline"
            onClick={() => onOpenChange(false)}
            disabled={isStarting}
          >
            Cancel
          </Button>
          <Button type="button" onClick={onConfirm} disabled={isStarting} aria-busy={isStarting}>
            {isStarting ? "Starting…" : "Start"}
          </Button>
        </DialogFooter>
      </DialogContent>
    </Dialog>
  );
}

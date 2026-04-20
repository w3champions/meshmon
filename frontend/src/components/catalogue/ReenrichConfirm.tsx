import * as DialogPrimitive from "@radix-ui/react-dialog";
import { Button } from "@/components/ui/button";

export interface ReenrichConfirmProps {
  /** Number of rows the caller is about to re-enrich. */
  selectionSize: number;
  /** Controls visibility. Parent is responsible for threshold gating. */
  open: boolean;
  /** Fires when the operator confirms the re-enrich action. */
  onConfirm: () => void;
  /** Fires when the operator dismisses the dialog. */
  onCancel: () => void;
}

/**
 * Renders a modal confirmation dialog for a bulk re-enrich action.
 *
 * The component is intentionally dumb: the parent owns the selection-size
 * threshold (25 rows matches the server's bulk guard) and simply toggles the
 * `open` prop when confirmation is warranted. When `open === false` the
 * dialog contents are not present in the DOM.
 */
export function ReenrichConfirm({
  selectionSize,
  open,
  onConfirm,
  onCancel,
}: ReenrichConfirmProps) {
  return (
    <DialogPrimitive.Root
      open={open}
      onOpenChange={(next) => {
        if (!next) onCancel();
      }}
    >
      <DialogPrimitive.Portal>
        <DialogPrimitive.Overlay className="fixed inset-0 z-[1100] bg-black/80 data-[state=open]:animate-in data-[state=closed]:animate-out data-[state=closed]:fade-out-0 data-[state=open]:fade-in-0" />
        <DialogPrimitive.Content
          className="fixed left-1/2 top-1/2 z-[1100] w-[92vw] max-w-md -translate-x-1/2 -translate-y-1/2 rounded-lg border bg-background p-6 shadow-lg focus:outline-none"
          aria-describedby="reenrich-confirm-body"
        >
          <DialogPrimitive.Title className="text-lg font-semibold text-foreground">
            Re-enrich {selectionSize} rows?
          </DialogPrimitive.Title>
          <DialogPrimitive.Description
            id="reenrich-confirm-body"
            className="mt-2 text-sm text-muted-foreground"
          >
            This will consume ~{selectionSize} ipgeolocation credits.
          </DialogPrimitive.Description>
          <div className="mt-6 flex justify-end gap-2">
            <Button variant="outline" onClick={onCancel} type="button">
              Cancel
            </Button>
            <Button onClick={onConfirm} type="button">
              Re-enrich
            </Button>
          </div>
        </DialogPrimitive.Content>
      </DialogPrimitive.Portal>
    </DialogPrimitive.Root>
  );
}

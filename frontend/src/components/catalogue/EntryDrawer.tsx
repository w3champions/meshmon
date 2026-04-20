import { zodResolver } from "@hookform/resolvers/zod";
import { Fragment, useEffect, useRef, useState } from "react";
import { Controller, useForm } from "react-hook-form";
import { toast } from "sonner";
import { z } from "zod";
import {
  type CatalogueEntry,
  type CataloguePatchRequest,
  useDeleteCatalogueEntry,
  usePatchCatalogueEntry,
  useReenrichOne,
} from "@/api/hooks/catalogue";
import { StatusChip } from "@/components/catalogue/StatusChip";
import { LocationPicker } from "@/components/map/LocationPicker";
import { Badge } from "@/components/ui/badge";
import { Button } from "@/components/ui/button";
import {
  Dialog,
  DialogContent,
  DialogDescription,
  DialogFooter,
  DialogHeader,
  DialogTitle,
} from "@/components/ui/dialog";
import { Input } from "@/components/ui/input";
import { Label } from "@/components/ui/label";
import {
  Select,
  SelectContent,
  SelectItem,
  SelectTrigger,
  SelectValue,
} from "@/components/ui/select";
import { COUNTRIES } from "@/lib/countries";

export interface EntryDrawerProps {
  /** Undefined closes the dialog. A defined entry opens it and seeds the form. */
  entry: CatalogueEntry | undefined;
  /** Fires when the dialog should close (overlay click, escape, explicit close). */
  onClose: () => void;
}

/**
 * Editable form fields on the dialog. Keys map 1:1 to `CatalogueEntryDto`
 * camelCase columns and drive both the Zod schema and the PascalCase map.
 */
type EditableField =
  | "display_name"
  | "asn"
  | "country_code"
  | "city"
  | "latitude"
  | "longitude"
  | "network_operator"
  | "website"
  | "notes";

/**
 * Maps each editable field to the PascalCase name the server expects in
 * `operator_edited_fields` and `revert_to_auto`. Mirrors the Rust `Field`
 * enum in `crates/service/src/catalogue/model.rs`.
 */
const FIELD_PASCAL_MAP: Record<EditableField, string> = {
  display_name: "DisplayName",
  asn: "Asn",
  country_code: "CountryCode",
  city: "City",
  latitude: "Latitude",
  longitude: "Longitude",
  network_operator: "NetworkOperator",
  website: "Website",
  notes: "Notes",
};

interface EditableFieldConfig {
  field: EditableField;
  label: string;
  /** When true the field spans both grid columns on sm+ viewports. */
  colSpan?: boolean;
  /** When true, renders a resizable textarea instead of a single-line input. */
  multiline?: boolean;
  /** When true, renders a country Select picker instead of an Input. */
  countrySelect?: boolean;
  extraProps?: Omit<React.ComponentProps<typeof Input>, "name" | "ref">;
}

/**
 * Ordered render-and-serialisation list for editable fields. The array order
 * is the visible order in the dialog; it also drives `buildPatchBody` so the
 * diff traversal stays stable.
 */
/**
 * Rendered editable rows — `latitude` and `longitude` are **not** in
 * this list because they render through a composite `<LocationRow>`
 * below. They remain in [`EditableField`] / `FIELD_PASCAL_MAP` /
 * [`buildPatchBody`] so the PATCH wire still carries each half
 * independently and the lock set stays accurate.
 */
const EDITABLE_FIELD_CONFIGS: readonly EditableFieldConfig[] = [
  { field: "display_name", label: "Display name" },
  {
    field: "asn",
    label: "ASN",
    extraProps: { type: "number", inputMode: "numeric" },
  },
  { field: "country_code", label: "Country", countrySelect: true },
  { field: "city", label: "City" },
  { field: "network_operator", label: "Network operator" },
  { field: "website", label: "Website" },
  { field: "notes", label: "Notes", colSpan: true, multiline: true },
];

/**
 * Every editable column in the PATCH traversal order. `buildPatchBody`
 * walks this list and picks up whichever fields RHF flagged as dirty;
 * `latitude` and `longitude` are appended here (not in
 * [`EDITABLE_FIELD_CONFIGS`]) because the composite `<LocationSection>`
 * renders them as a single surface while still flowing as two
 * independent PATCH slots on the wire.
 */
const EDITABLE_FIELDS: readonly EditableField[] = [
  ...EDITABLE_FIELD_CONFIGS.map((c) => c.field),
  "latitude",
  "longitude",
];

const numberFromInput = z.union([z.number(), z.string()]).transform((value) => {
  if (typeof value === "number") return value;
  if (value.trim() === "") return "";
  const parsed = Number(value);
  return Number.isFinite(parsed) ? parsed : Number.NaN;
});

const schema = z.object({
  display_name: z.string(),
  asn: numberFromInput.refine(
    (v) => v === "" || (Number.isInteger(v) && v >= 0 && v <= 2 ** 32 - 1),
    { message: "ASN must be a non-negative 32-bit integer" },
  ),
  country_code: z.string(),
  city: z.string(),
  latitude: numberFromInput.refine((v) => v === "" || (v >= -90 && v <= 90), {
    message: "Latitude must be between -90 and 90",
  }),
  longitude: numberFromInput.refine((v) => v === "" || (v >= -180 && v <= 180), {
    message: "Longitude must be between -180 and 180",
  }),
  network_operator: z.string(),
  website: z.string(),
  notes: z.string(),
});

/**
 * RHF form shape derived from the Zod schema. Numeric fields are typed as
 * `number | string` because empty text inputs produce `""` before Zod's
 * transform narrows them; `toPatchValue` normalises both to `null` on the
 * wire.
 */
type FormValues = z.input<typeof schema>;

function toFormValues(entry: CatalogueEntry): FormValues {
  return {
    display_name: entry.display_name ?? "",
    asn: entry.asn ?? "",
    country_code: entry.country_code ?? "",
    city: entry.city ?? "",
    latitude: entry.latitude ?? "",
    longitude: entry.longitude ?? "",
    network_operator: entry.network_operator ?? "",
    website: entry.website ?? "",
    notes: entry.notes ?? "",
  };
}

/**
 * Converts a single editable form value to the PATCH wire shape. Empty
 * strings become `null` (server: "set NULL") and whitespace-only strings
 * for text fields are normalised to `null` as well.
 */
function toPatchValue(
  field: EditableField,
  value: FormValues[EditableField],
): string | number | null {
  if (field === "asn" || field === "latitude" || field === "longitude") {
    if (value === "" || value === undefined || value === null) return null;
    return typeof value === "number" ? value : Number(value);
  }
  const str = typeof value === "string" ? value : String(value ?? "");
  const trimmed = str.trim();
  return trimmed === "" ? null : trimmed;
}

/**
 * Builds a PATCH body containing only the fields that are dirty in the RHF
 * form, mapped to the triple-state wire shape. Returns `undefined` when
 * there are no changes so the caller can skip the mutation entirely.
 */
function buildPatchBody(
  values: FormValues,
  dirty: Partial<Record<EditableField, boolean>>,
): CataloguePatchRequest | undefined {
  const body: CataloguePatchRequest = {};
  let touched = false;
  for (const field of EDITABLE_FIELDS) {
    if (!dirty[field]) continue;
    touched = true;
    const patched = toPatchValue(field, values[field]);
    // `CataloguePatchRequest` uses `field?: T | null` for every editable
    // column — narrow to the exact slot for this field.
    (body as Record<string, string | number | null>)[field] = patched;
  }
  return touched ? body : undefined;
}

/**
 * Returns `prefix: err.message` when a useful message is available, otherwise
 * just `prefix`. Keeps toast copy consistent across the dialog's three
 * mutations without relying on a portal-rendered custom component.
 */
function toastMessage(prefix: string, err: unknown): string {
  if (err instanceof Error && err.message) return `${prefix}: ${err.message}`;
  return prefix;
}

function formatTimestamp(value: string | null | undefined): string {
  if (!value) return "—";
  try {
    return new Date(value)
      .toISOString()
      .replace("T", " ")
      .replace(/\.\d+Z$/, "Z");
  } catch {
    return value;
  }
}

interface ReadonlyRowProps {
  label: string;
  children: React.ReactNode;
}

function ReadonlyRow({ label, children }: ReadonlyRowProps) {
  return (
    <div className="grid grid-cols-[8rem_1fr] items-center gap-2 text-sm">
      <span className="text-muted-foreground">{label}</span>
      <span className="text-foreground">{children}</span>
    </div>
  );
}

const TEXTAREA_CLASS =
  "w-full rounded-md border border-input bg-background px-3 py-2 text-sm ring-offset-background placeholder:text-muted-foreground focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ring disabled:cursor-not-allowed disabled:opacity-50 resize-none max-h-40 overflow-y-auto";

interface EditableFieldRowProps {
  field: EditableField;
  label: string;
  colSpan?: boolean;
  multiline?: boolean;
  countrySelect?: boolean;
  locked: boolean;
  isPending: boolean;
  errorMessage?: string;
  inputProps: React.ComponentProps<typeof Input>;
  /** Value for the country select (only used when countrySelect=true). */
  countryValue?: string;
  /** onChange handler for the country select. */
  onCountryChange?: (value: string) => void;
  onRevert: (field: EditableField) => void;
}

function EditableFieldRow({
  field,
  label,
  colSpan,
  multiline,
  countrySelect,
  locked,
  isPending,
  errorMessage,
  inputProps,
  countryValue,
  onCountryChange,
  onRevert,
}: EditableFieldRowProps) {
  const id = `entry-drawer-${field}`;
  return (
    <div className={colSpan ? "space-y-1 sm:col-span-2" : "space-y-1"}>
      <div className="flex items-center justify-between">
        <div className="flex items-center gap-2">
          <Label htmlFor={id}>{label}</Label>
          {locked && (
            <Badge variant="secondary" className="text-[10px] px-1.5 py-0 leading-5">
              Operator-edited
            </Badge>
          )}
        </div>
        {locked && (
          <button
            type="button"
            onClick={() => onRevert(field)}
            disabled={isPending}
            className="text-xs text-primary underline-offset-2 hover:underline disabled:cursor-not-allowed disabled:opacity-50"
          >
            Revert to auto
          </button>
        )}
      </div>
      {countrySelect ? (
        <Select
          // Radix Select forbids empty string values on SelectItem, so we use a
          // sentinel "__none__" for the "clear" option and translate on the boundary.
          value={countryValue === "" || countryValue == null ? "__none__" : countryValue}
          onValueChange={(val) => onCountryChange?.(val === "__none__" ? "" : val)}
          disabled={isPending}
        >
          <SelectTrigger id={id} className={locked ? "ring-1 ring-primary/30" : undefined}>
            <SelectValue placeholder="Select a country…" />
          </SelectTrigger>
          <SelectContent className="max-h-72">
            {/* Clear option — sentinel value because Radix forbids empty strings */}
            <SelectItem value="__none__">— (none)</SelectItem>
            {COUNTRIES.map((c) => (
              <SelectItem key={c.code} value={c.code}>
                {c.name} ({c.code})
              </SelectItem>
            ))}
          </SelectContent>
        </Select>
      ) : multiline ? (
        <textarea
          id={id}
          rows={4}
          {...(inputProps as React.ComponentProps<"textarea">)}
          className={`${TEXTAREA_CLASS}${locked ? " ring-1 ring-primary/30" : ""}`}
        />
      ) : (
        <Input id={id} {...inputProps} className={locked ? "ring-1 ring-primary/30" : undefined} />
      )}
      {errorMessage && <p className="text-xs text-destructive">{errorMessage}</p>}
    </div>
  );
}

interface DeleteConfirmDialogProps {
  open: boolean;
  pending: boolean;
  onConfirm: () => void;
  onCancel: () => void;
}

function DeleteConfirmDialog({ open, pending, onConfirm, onCancel }: DeleteConfirmDialogProps) {
  const confirmRef = useRef<HTMLButtonElement>(null);

  return (
    <Dialog open={open} onOpenChange={(next) => !next && onCancel()}>
      <DialogContent
        className="sm:max-w-md"
        onOpenAutoFocus={(e) => {
          e.preventDefault();
          confirmRef.current?.focus();
        }}
      >
        <DialogHeader>
          <DialogTitle>Delete this catalogue entry?</DialogTitle>
          <DialogDescription>
            This removes the row and its enrichment history. This action cannot be undone.
          </DialogDescription>
        </DialogHeader>
        <DialogFooter>
          <Button variant="outline" size="sm" type="button" onClick={onCancel} disabled={pending}>
            Cancel
          </Button>
          <Button
            ref={confirmRef}
            variant="destructive"
            size="sm"
            type="button"
            onClick={onConfirm}
            disabled={pending}
          >
            Confirm delete
          </Button>
        </DialogFooter>
      </DialogContent>
    </Dialog>
  );
}

/**
 * Renders the operator-edit dialog for a single catalogue entry. The parent
 * supplies the current `entry` (undefined closes the dialog); inside, a
 * react-hook-form form manages editable columns, exposes per-field
 * "Revert to auto" links for operator-locked fields, and issues diff-only
 * PATCH requests via `usePatchCatalogueEntry`.
 */
export function EntryDrawer({ entry, onClose }: EntryDrawerProps) {
  const open = entry !== undefined;

  const patchMutation = usePatchCatalogueEntry();
  const reenrichMutation = useReenrichOne();
  const deleteMutation = useDeleteCatalogueEntry();

  const [deleteOpen, setDeleteOpen] = useState(false);

  const {
    register,
    control,
    handleSubmit,
    reset,
    getValues,
    formState: { dirtyFields, errors },
  } = useForm<FormValues>({
    resolver: zodResolver(schema),
    defaultValues: entry ? toFormValues(entry) : toFormValues(EMPTY_ENTRY),
  });

  // Re-seed the form whenever a new entry is opened so dirtyFields starts
  // clean and "Revert to auto" behaves against the latest lock state.
  useEffect(() => {
    if (entry) {
      reset(toFormValues(entry));
      setDeleteOpen(false);
    }
  }, [entry, reset]);

  if (!entry) {
    return (
      <Dialog open={false} onOpenChange={(next) => !next && onClose()}>
        <DialogContent />
      </Dialog>
    );
  }

  const lockedFields = new Set(entry.operator_edited_fields);

  const handleRevert = (field: EditableField): void => {
    const pascal = FIELD_PASCAL_MAP[field];
    const patch: CataloguePatchRequest = {
      revert_to_auto: [pascal],
      [field]: null,
    } as CataloguePatchRequest;
    patchMutation.mutate(
      { id: entry.id, patch },
      {
        onError: (err) => {
          toast.error(toastMessage("Couldn't revert to auto", err));
        },
      },
    );
    // Clear the local form value so the diff reflects the revert and the
    // input mirrors the server's nulled column on echo.
    const next = { ...getValues(), [field]: "" } as FormValues;
    reset(next, { keepDirty: false });
  };

  /**
   * Paired-revert helper for the composite Location row. Lat and Lng
   * are two columns on the wire but one editing surface in the UI —
   * reverting one half without the other would leave a half-locked
   * pair that a bulk paste cannot safely populate.
   */
  const handleRevertLocation = (): void => {
    const patch: CataloguePatchRequest = {
      revert_to_auto: ["Latitude", "Longitude"],
      latitude: null,
      longitude: null,
    };
    patchMutation.mutate(
      { id: entry.id, patch },
      {
        onError: (err) => {
          toast.error(toastMessage("Couldn't revert to auto", err));
        },
      },
    );
    const next = {
      ...getValues(),
      latitude: "",
      longitude: "",
    } as FormValues;
    reset(next, { keepDirty: false });
  };

  const onSubmit = handleSubmit((values) => {
    const dirty = dirtyFields as Partial<Record<EditableField, boolean>>;
    const body = buildPatchBody(values, dirty);
    if (!body) return;
    patchMutation.mutate(
      { id: entry.id, patch: body },
      {
        onSuccess: (updated) => {
          reset(toFormValues(updated));
        },
        onError: (err) => {
          toast.error(toastMessage("Couldn't save changes", err));
        },
      },
    );
  });

  const handleReenrich = (): void => {
    reenrichMutation.mutate(entry.id, {
      onError: (err) => {
        toast.error(toastMessage("Couldn't re-enrich entry", err));
      },
    });
  };

  const handleConfirmDelete = (): void => {
    deleteMutation.mutate(entry.id, {
      onSuccess: () => {
        setDeleteOpen(false);
        onClose();
      },
      onError: (err) => {
        toast.error(toastMessage("Couldn't delete entry", err));
      },
    });
  };

  return (
    <Dialog open={open} onOpenChange={(next) => !next && onClose()}>
      <DialogContent
        // `!grid` overrides the Radix `DialogContent` default so the
        // outer content box stacks header / scroll-area / footer as a
        // flex column. The footer stays on-screen while the middle
        // section owns the `overflow-y-auto`, so Save / Re-enrich /
        // Delete never get hidden under the dialog chrome.
        className="w-[95vw] sm:max-w-3xl max-h-[90vh] !grid-rows-none !grid-cols-none !grid-flow-row !gap-0 flex flex-col p-0"
        aria-label="Catalogue entry editor"
      >
        <DialogHeader className="px-6 pt-6 pb-2">
          <DialogTitle>Edit catalogue entry</DialogTitle>
          <DialogDescription>
            Operator edits lock individual fields against automatic enrichment.
          </DialogDescription>
        </DialogHeader>

        <form onSubmit={onSubmit} className="flex flex-col min-h-0 flex-1">
          <div className="flex-1 min-h-0 overflow-y-auto px-6 pb-4 space-y-4">
            <section className="rounded-md border bg-muted/30 p-3">
              <div className="grid grid-cols-1 sm:grid-cols-2 gap-x-6 gap-y-2">
                <ReadonlyRow label="IP">{entry.ip}</ReadonlyRow>
                <ReadonlyRow label="Created">{formatTimestamp(entry.created_at)}</ReadonlyRow>
                <ReadonlyRow label="Status">
                  <StatusChip status={entry.enrichment_status} />
                </ReadonlyRow>
                <ReadonlyRow label="Created by">{entry.created_by ?? "—"}</ReadonlyRow>
                <ReadonlyRow label="Source">{entry.source}</ReadonlyRow>
                <ReadonlyRow label="Enriched at">{formatTimestamp(entry.enriched_at)}</ReadonlyRow>
              </div>
            </section>

            <section className="grid grid-cols-1 sm:grid-cols-2 gap-4">
              {EDITABLE_FIELD_CONFIGS.map(
                ({ field, label, colSpan, multiline, countrySelect, extraProps }) => {
                  // Notes is the last field in the config; render the
                  // composite Location row immediately before it so the
                  // big map lives near the bottom of the form where it
                  // doesn't push the quick text fields below the fold.
                  const locationBeforeNotes =
                    field === "notes" ? (
                      <LocationSection
                        key="location-section"
                        control={control}
                        locked={lockedFields.has("Latitude") || lockedFields.has("Longitude")}
                        isPending={patchMutation.isPending}
                        onRevert={handleRevertLocation}
                      />
                    ) : null;
                  const row = countrySelect ? (
                    <Controller
                      key={field}
                      name={field as "country_code"}
                      control={control}
                      render={({ field: controllerField }) => (
                        <EditableFieldRow
                          field={field}
                          label={label}
                          colSpan={colSpan}
                          countrySelect
                          locked={lockedFields.has(FIELD_PASCAL_MAP[field])}
                          isPending={patchMutation.isPending}
                          errorMessage={errors[field]?.message}
                          inputProps={{}}
                          countryValue={
                            typeof controllerField.value === "string" ? controllerField.value : ""
                          }
                          onCountryChange={controllerField.onChange}
                          onRevert={handleRevert}
                        />
                      )}
                    />
                  ) : (
                    <EditableFieldRow
                      key={field}
                      field={field}
                      label={label}
                      colSpan={colSpan}
                      multiline={multiline}
                      locked={lockedFields.has(FIELD_PASCAL_MAP[field])}
                      isPending={patchMutation.isPending}
                      errorMessage={errors[field]?.message}
                      inputProps={{ ...(extraProps ?? {}), ...register(field) }}
                      onRevert={handleRevert}
                    />
                  );
                  return locationBeforeNotes ? (
                    <Fragment key={`${field}-with-location`}>
                      {locationBeforeNotes}
                      {row}
                    </Fragment>
                  ) : (
                    row
                  );
                },
              )}
            </section>
          </div>

          <DialogFooter className="flex flex-col gap-2 sm:flex-row sm:justify-between border-t bg-background px-6 py-4">
            <div className="flex gap-2">
              <Button
                type="button"
                variant="outline"
                onClick={handleReenrich}
                disabled={reenrichMutation.isPending}
              >
                Re-enrich
              </Button>
              <Button
                type="button"
                variant="destructive"
                onClick={() => setDeleteOpen(true)}
                disabled={deleteMutation.isPending}
              >
                Delete
              </Button>
            </div>
            <Button type="submit" disabled={patchMutation.isPending}>
              Save
            </Button>
          </DialogFooter>
        </form>

        <DeleteConfirmDialog
          open={deleteOpen}
          pending={deleteMutation.isPending}
          onConfirm={handleConfirmDelete}
          onCancel={() => setDeleteOpen(false)}
        />
      </DialogContent>
    </Dialog>
  );
}

/**
 * Zero-value entry used solely to seed RHF defaults when the dialog opens
 * with `entry === undefined`. It is never rendered.
 */
const EMPTY_ENTRY: CatalogueEntry = {
  id: "",
  ip: "",
  created_at: "",
  source: "operator",
  enrichment_status: "pending",
  operator_edited_fields: [],
};

interface LocationSectionProps {
  control: ReturnType<typeof useForm<FormValues>>["control"];
  locked: boolean;
  isPending: boolean;
  onRevert(): void;
}

/**
 * Composite Latitude+Longitude row rendered via the reusable
 * [`LocationPicker`]. The two halves remain separate on the PATCH
 * wire, but the operator sees a single editing surface — clicks,
 * drags, and reverts apply atomically, matching the backend's
 * paired-atomicity rule.
 *
 * Lives outside the form grid because the picker needs the full dialog
 * width; the grid's two-column layout would squeeze the map into a
 * sub-usable aspect ratio.
 */
function LocationSection({ control, locked, isPending, onRevert }: LocationSectionProps) {
  return (
    <section className="space-y-1 sm:col-span-2">
      <div className="flex items-center justify-between">
        <div className="flex items-center gap-2">
          <span className="text-sm font-medium">Location</span>
          {locked && (
            <Badge variant="secondary" className="text-[10px] px-1.5 py-0 leading-5">
              Operator-edited
            </Badge>
          )}
        </div>
        {locked && (
          <button
            type="button"
            onClick={onRevert}
            disabled={isPending}
            className="text-xs text-primary underline-offset-2 hover:underline disabled:cursor-not-allowed disabled:opacity-50"
          >
            Revert to auto
          </button>
        )}
      </div>
      <Controller
        control={control}
        name="latitude"
        render={({ field: latField }) => (
          <Controller
            control={control}
            name="longitude"
            render={({ field: lonField }) => {
              const latNum =
                typeof latField.value === "number"
                  ? latField.value
                  : latField.value === "" || latField.value === undefined
                    ? Number.NaN
                    : Number(latField.value);
              const lonNum =
                typeof lonField.value === "number"
                  ? lonField.value
                  : lonField.value === "" || lonField.value === undefined
                    ? Number.NaN
                    : Number(lonField.value);
              const value =
                Number.isFinite(latNum) && Number.isFinite(lonNum)
                  ? { latitude: latNum, longitude: lonNum }
                  : null;
              return (
                <LocationPicker
                  value={value}
                  onChange={(next) => {
                    // Route through each `field.onChange` so RHF flags
                    // both halves as dirty. A composite change to
                    // Location must carry both `latitude` and
                    // `longitude` into the next PATCH body.
                    latField.onChange(next ? next.latitude : "");
                    lonField.onChange(next ? next.longitude : "");
                  }}
                />
              );
            }}
          />
        )}
      />
    </section>
  );
}

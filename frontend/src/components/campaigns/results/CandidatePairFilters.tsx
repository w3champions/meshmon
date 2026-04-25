/**
 * Sticky filter toolbar for the candidates drilldown dialog.
 *
 * Four numeric inputs (`min_improvement_ms`, `min_improvement_ratio`,
 * `max_transit_rtt_ms`, `max_transit_stddev_ms`) and a "Qualifies only"
 * toggle. Empty inputs collapse to `null` so the wire query param is
 * omitted; negative values round-trip cleanly so signed thresholds
 * (matching the I2 evaluator semantics) work end-to-end.
 *
 * Inputs are debounced by ~250 ms before they propagate up to the
 * dialog — typing fast doesn't spam the backend, but the lag is short
 * enough to feel snappy. The `value` prop is the canonical state and
 * the input's local state seeds from it on prop changes (e.g. "Reset
 * filters" button click) without breaking the debounce loop.
 */

import { useCallback, useEffect, useId, useRef, useState } from "react";
import type { PairDetailsQuery } from "@/api/hooks/evaluation-pairs";
import { Button } from "@/components/ui/button";
import { Input } from "@/components/ui/input";
import { Label } from "@/components/ui/label";

const DEBOUNCE_MS = 250;

export interface CandidatePairFiltersProps {
  value: PairDetailsQuery;
  onChange: (next: PairDetailsQuery) => void;
  /**
   * Active campaign-level guardrails. When set, each numeric input
   * renders `≥/≤ <n> (guardrail)` as its placeholder so the operator
   * sees what the storage / eligibility filter is already enforcing
   * before they layer a runtime narrowing on top.
   */
  guardrails: {
    min_improvement_ms: number | null;
    min_improvement_ratio: number | null;
    max_transit_rtt_ms: number | null;
    max_transit_stddev_ms: number | null;
  };
}

/** Parse "" → null, otherwise `Number(...)`. Returns `null` for `NaN`. */
function parseNullable(input: string): number | null {
  if (input.trim() === "") return null;
  const n = Number(input);
  if (Number.isNaN(n)) return null;
  return n;
}

/** Format a numeric value back into an input string; `null` ⇒ "". */
function formatNullable(value: number | null | undefined): string {
  if (value === null || value === undefined) return "";
  return String(value);
}

export function CandidatePairFilters({ value, onChange, guardrails }: CandidatePairFiltersProps) {
  // Local input state — the canonical state lives in `value`, but we
  // mirror it here so the user can type without the parent re-committing
  // on every keystroke. The debounced effect below pushes parsed values
  // back up after `DEBOUNCE_MS` of idle.
  const [minMs, setMinMs] = useState(() => formatNullable(value.min_improvement_ms));
  const [minRatio, setMinRatio] = useState(() => formatNullable(value.min_improvement_ratio));
  const [maxRtt, setMaxRtt] = useState(() => formatNullable(value.max_transit_rtt_ms));
  const [maxSd, setMaxSd] = useState(() => formatNullable(value.max_transit_stddev_ms));

  // Re-seed local state when the parent resets the canonical query
  // (e.g. "Reset filters" button). String compare is enough because
  // `formatNullable` is deterministic per value.
  useEffect(() => {
    setMinMs(formatNullable(value.min_improvement_ms));
  }, [value.min_improvement_ms]);
  useEffect(() => {
    setMinRatio(formatNullable(value.min_improvement_ratio));
  }, [value.min_improvement_ratio]);
  useEffect(() => {
    setMaxRtt(formatNullable(value.max_transit_rtt_ms));
  }, [value.max_transit_rtt_ms]);
  useEffect(() => {
    setMaxSd(formatNullable(value.max_transit_stddev_ms));
  }, [value.max_transit_stddev_ms]);

  // Stable refs to the latest `value` and `onChange` so the debounce
  // effects don't tear down their timer on every render.
  const valueRef = useRef(value);
  valueRef.current = value;
  const onChangeRef = useRef(onChange);
  onChangeRef.current = onChange;

  const commit = useCallback((patch: Partial<PairDetailsQuery>) => {
    onChangeRef.current({ ...valueRef.current, ...patch });
  }, []);

  // Debounce each input independently so a fast typist doesn't queue
  // up four refetches per character.
  useEffect(() => {
    const parsed = parseNullable(minMs);
    if (parsed === valueRef.current.min_improvement_ms) return;
    const handle = window.setTimeout(() => commit({ min_improvement_ms: parsed }), DEBOUNCE_MS);
    return () => window.clearTimeout(handle);
  }, [minMs, commit]);
  useEffect(() => {
    const parsed = parseNullable(minRatio);
    if (parsed === valueRef.current.min_improvement_ratio) return;
    const handle = window.setTimeout(() => commit({ min_improvement_ratio: parsed }), DEBOUNCE_MS);
    return () => window.clearTimeout(handle);
  }, [minRatio, commit]);
  useEffect(() => {
    const parsed = parseNullable(maxRtt);
    if (parsed === valueRef.current.max_transit_rtt_ms) return;
    const handle = window.setTimeout(() => commit({ max_transit_rtt_ms: parsed }), DEBOUNCE_MS);
    return () => window.clearTimeout(handle);
  }, [maxRtt, commit]);
  useEffect(() => {
    const parsed = parseNullable(maxSd);
    if (parsed === valueRef.current.max_transit_stddev_ms) return;
    const handle = window.setTimeout(() => commit({ max_transit_stddev_ms: parsed }), DEBOUNCE_MS);
    return () => window.clearTimeout(handle);
  }, [maxSd, commit]);

  const minMsId = useId();
  const minRatioId = useId();
  const maxRttId = useId();
  const maxSdId = useId();
  const qualifiesId = useId();

  const minMsPlaceholder =
    guardrails.min_improvement_ms != null
      ? `≥ ${guardrails.min_improvement_ms} ms (guardrail)`
      : "≥ … ms";
  const minRatioPlaceholder =
    guardrails.min_improvement_ratio != null
      ? `≥ ${guardrails.min_improvement_ratio} (guardrail)`
      : "≥ … ratio";
  const maxRttPlaceholder =
    guardrails.max_transit_rtt_ms != null
      ? `≤ ${guardrails.max_transit_rtt_ms} ms (guardrail)`
      : "≤ … ms";
  const maxSdPlaceholder =
    guardrails.max_transit_stddev_ms != null
      ? `≤ ${guardrails.max_transit_stddev_ms} ms (guardrail)`
      : "≤ … ms";

  const handleReset = useCallback(() => {
    setMinMs("");
    setMinRatio("");
    setMaxRtt("");
    setMaxSd("");
    onChangeRef.current({
      ...valueRef.current,
      min_improvement_ms: null,
      min_improvement_ratio: null,
      max_transit_rtt_ms: null,
      max_transit_stddev_ms: null,
      qualifies_only: null,
    });
  }, []);

  const handleQualifiesToggle = useCallback(() => {
    const current = valueRef.current.qualifies_only ?? false;
    onChangeRef.current({
      ...valueRef.current,
      qualifies_only: current ? null : true,
    });
  }, []);

  const qualifiesOn = value.qualifies_only === true;

  return (
    <section
      aria-label="Pair filters"
      className="sticky top-0 z-10 flex flex-wrap items-end gap-3 border-b bg-background/95 px-4 py-3 backdrop-blur"
    >
      <FilterField id={minMsId} label="Min Δ ms">
        <Input
          id={minMsId}
          type="number"
          inputMode="decimal"
          step="any"
          placeholder={minMsPlaceholder}
          value={minMs}
          onChange={(e) => setMinMs(e.target.value)}
          className="h-8 w-40"
          data-testid="filter-min-improvement-ms"
        />
      </FilterField>
      <FilterField id={minRatioId} label="Min Δ ratio">
        <Input
          id={minRatioId}
          type="number"
          inputMode="decimal"
          step="any"
          placeholder={minRatioPlaceholder}
          value={minRatio}
          onChange={(e) => setMinRatio(e.target.value)}
          className="h-8 w-40"
          data-testid="filter-min-improvement-ratio"
        />
      </FilterField>
      <FilterField id={maxRttId} label="Max transit RTT">
        <Input
          id={maxRttId}
          type="number"
          inputMode="decimal"
          step="any"
          placeholder={maxRttPlaceholder}
          value={maxRtt}
          onChange={(e) => setMaxRtt(e.target.value)}
          className="h-8 w-40"
          data-testid="filter-max-transit-rtt-ms"
        />
      </FilterField>
      <FilterField id={maxSdId} label="Max transit stddev">
        <Input
          id={maxSdId}
          type="number"
          inputMode="decimal"
          step="any"
          placeholder={maxSdPlaceholder}
          value={maxSd}
          onChange={(e) => setMaxSd(e.target.value)}
          className="h-8 w-40"
          data-testid="filter-max-transit-stddev-ms"
        />
      </FilterField>
      <div className="flex items-center gap-2">
        <input
          id={qualifiesId}
          type="checkbox"
          role="switch"
          aria-checked={qualifiesOn}
          checked={qualifiesOn}
          onChange={handleQualifiesToggle}
          className="h-4 w-4 cursor-pointer accent-primary"
          data-testid="filter-qualifies-only"
        />
        <Label htmlFor={qualifiesId} className="cursor-pointer text-xs">
          Qualifies only
        </Label>
      </div>
      <Button
        type="button"
        variant="link"
        size="sm"
        className="h-8 px-1 text-xs"
        onClick={handleReset}
        data-testid="filter-reset"
      >
        Reset filters
      </Button>
    </section>
  );
}

interface FilterFieldProps {
  id: string;
  label: string;
  children: React.ReactNode;
}

function FilterField({ id, label, children }: FilterFieldProps) {
  return (
    <div className="flex flex-col gap-1">
      <Label htmlFor={id} className="text-[10px] uppercase tracking-wide text-muted-foreground">
        {label}
      </Label>
      {children}
    </div>
  );
}

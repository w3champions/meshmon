import "./leaflet-setup";

import type { LeafletMouseEvent } from "leaflet";
import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import { MapContainer, Marker, TileLayer, useMap, useMapEvents } from "react-leaflet";
import { Button } from "@/components/ui/button";
import { cn } from "@/lib/utils";
import { useUiStore } from "@/stores/ui";

/**
 * Single coordinate the operator has chosen via map click, marker drag,
 * or an external caller. Both components that use this picker —
 * `PasteStaging`'s Add IPs dialog and the `EntryDrawer`'s Location row —
 * pass the same shape through to their respective wire formats.
 */
export interface LocationPickerValue {
  latitude: number;
  longitude: number;
}

export interface LocationPickerProps {
  /**
   * Current location (or `null` when none is selected). The component
   * is fully controlled; render-time changes to `value` re-render the
   * marker in place.
   */
  value: LocationPickerValue | null;
  /**
   * Emitted for every user intent that changes the location:
   *  - map click → `{latitude, longitude}` of the clicked point.
   *  - marker drag end → the new marker position.
   *  - Clear button → `null`.
   */
  onChange(next: LocationPickerValue | null): void;
  /** Additional classes applied to the outer shell. */
  className?: string;
  /** Accessible label for the Clear button. */
  clearLabel?: string;
  /** Height class for the map. Defaults to `h-64`. */
  heightClassName?: string;
}

// CARTO's Voyager (light) and Dark Matter (dark) basemaps — same URLs
// DrawMap uses so the picker shares the fleet's visual language.
const TILE_URL_LIGHT = "https://{s}.basemaps.cartocdn.com/rastertiles/voyager/{z}/{x}/{y}{r}.png";
const TILE_URL_DARK = "https://{s}.basemaps.cartocdn.com/dark_all/{z}/{x}/{y}{r}.png";
const TILE_ATTRIBUTION =
  '© <a href="https://www.openstreetmap.org/copyright">OpenStreetMap</a> contributors © <a href="https://carto.com/attributions">CARTO</a>';
const TILE_SUBDOMAINS = "abcd";

// World-overview centre used when no coordinate is selected. Matches
// DrawMap so operators switching between the two see the same default
// framing.
const DEFAULT_CENTER: [number, number] = [20, 0];
const DEFAULT_ZOOM = 2;

// How long the "hold ⌘/Ctrl to zoom" hint stays visible after a plain-
// wheel attempt. Matches DrawMap so the two pickers feel identical.
const ZOOM_HINT_FADE_MS = 1500;

function detectIsMac(): boolean {
  if (typeof navigator === "undefined") return false;
  const platform = (navigator as Navigator & { userAgentData?: { platform?: string } })
    .userAgentData?.platform;
  if (typeof platform === "string" && platform.length > 0) {
    return /mac/i.test(platform);
  }
  if (typeof navigator.platform === "string" && navigator.platform.length > 0) {
    return /mac/i.test(navigator.platform);
  }
  if (typeof navigator.userAgent === "string") {
    return /mac/i.test(navigator.userAgent);
  }
  return false;
}

/**
 * Format a signed decimal coordinate with four fractional digits —
 * same precision the Lat/Lng inputs in the entry drawer accepted.
 */
function formatCoord(value: number): string {
  return value.toFixed(4);
}

/**
 * Subscribe to map clicks. Lives inside `<MapContainer>` so the
 * `useMapEvents` hook has a live `L.Map` to attach to; parents never
 * render this directly.
 */
function ClickToPlace({ onPick }: { onPick(next: LocationPickerValue): void }) {
  useMapEvents({
    click: (event: LeafletMouseEvent) => {
      onPick({ latitude: event.latlng.lat, longitude: event.latlng.lng });
    },
  });
  return null;
}

/**
 * Zoom level used when the picker first receives a non-null value —
 * continent-level framing so the operator still sees surrounding
 * landmass and can orient themselves before fine-tuning the pick.
 * (Zoom 6 used to snap to mid-country and hid context.)
 */
const FIRST_PICK_ZOOM = 4;

/**
 * Keep the Leaflet viewport tracking controlled `value` updates.
 *
 * `react-leaflet`'s `MapContainer` reads `center`/`zoom` only on initial
 * mount; subsequent prop changes never reach the underlying `L.Map`. When
 * a parent updates `value` — e.g. the drawer navigates between entries
 * with different coordinates, or an external "revert to auto" nulls a
 * previous selection — the marker moves but the viewport would stay on
 * the old area, hiding the selected point off-screen. This helper bridges
 * the gap by calling `setView` whenever `value` changes, and returning
 * to the world-overview default when `value` clears.
 *
 * Zoom is preserved across point→point transitions so an operator who
 * zoomed in to fine-tune a pick does not get thrown back to the initial
 * zoom when they click a nearby spot or drag the marker. Only the
 * null→point (first selection) and point→null (clear) transitions
 * override the zoom.
 *
 * User-driven pans and zooms survive untouched: the effect only fires
 * when the controlled `value` prop itself changes, not on map
 * interaction.
 */
interface ModifierZoomControllerProps {
  onHintNeeded(): void;
}

/**
 * Default page scroll when hovering the map; modifier-gated wheel zoom.
 *
 * Matches DrawMap so the two map surfaces behave identically. Plain
 * scroll bubbles (the page scrolls) and flashes the hint overlay;
 * ⌘ (macOS) or Ctrl held consumes the wheel and nudges zoom.
 */
function ModifierZoomController({ onHintNeeded }: ModifierZoomControllerProps) {
  const map = useMap();
  const onHintNeededRef = useRef(onHintNeeded);
  useEffect(() => {
    onHintNeededRef.current = onHintNeeded;
  }, [onHintNeeded]);

  useEffect(() => {
    const container = map.getContainer();

    const handleWheel = (event: WheelEvent) => {
      if (event.ctrlKey || event.metaKey) {
        event.preventDefault();
        const delta = event.deltaY;
        if (delta === 0) return;
        const snap = map.options.zoomSnap ?? 1;
        const step = delta < 0 ? snap : -snap;
        map.setZoom(map.getZoom() + step);
        return;
      }
      onHintNeededRef.current();
    };

    container.addEventListener("wheel", handleWheel, { passive: false });
    return () => {
      container.removeEventListener("wheel", handleWheel);
    };
  }, [map]);

  return null;
}

function RecenterOnValueChange({ value }: { value: LocationPickerValue | null }) {
  const map = useMap();
  // Snapshot the value we last re-centred on so marker-drag round-trips
  // (which update `value` by sub-pixel amounts) don't fight the operator
  // mid-gesture. Compare by stringified coords — cheap and precise enough
  // for 1e-7° granularity.
  const last = useRef<string | null>(null);
  useEffect(() => {
    const key = value ? `${value.latitude},${value.longitude}` : null;
    if (last.current === key) return;
    const previousKey = last.current;
    last.current = key;
    if (!value) {
      map.setView(DEFAULT_CENTER, DEFAULT_ZOOM);
      return;
    }
    // null → point: zoom in from the world overview. Otherwise keep the
    // operator's current zoom so a nearby re-click doesn't throw them
    // out of a close-up framing.
    if (previousKey === null) {
      map.setView([value.latitude, value.longitude], FIRST_PICK_ZOOM);
    } else {
      map.setView([value.latitude, value.longitude]);
    }
  }, [map, value]);
  return null;
}

/**
 * Seed the mount-time viewport. We always open at
 * [`DEFAULT_CENTER`]+[`DEFAULT_ZOOM`] even when a `value` is already
 * supplied so the operator sees continent-level context; the
 * `RecenterOnValueChange` helper then animates in to the supplied
 * point at [`FIRST_PICK_ZOOM`] on the first effect tick. Centralising
 * this lets the tests keep asserting `null → point` transitions via
 * `__setViewCalls` rather than mount-time `center` snapshots.
 */

export function LocationPicker({
  value,
  onChange,
  className,
  clearLabel = "Clear location",
  heightClassName = "h-64",
}: LocationPickerProps) {
  const theme = useUiStore((s) => s.theme);
  const tileUrl = theme === "dark" ? TILE_URL_DARK : TILE_URL_LIGHT;

  // Respect reduced-motion by disabling zoom/marker animation when the
  // user has expressed the preference. Evaluated once on mount — the
  // media query rarely flips during a session and Leaflet does not
  // swap these options reactively anyway.
  const [animate, setAnimate] = useState(true);
  useEffect(() => {
    if (typeof window === "undefined" || typeof window.matchMedia !== "function") {
      return;
    }
    const mq = window.matchMedia("(prefers-reduced-motion: reduce)");
    setAnimate(!mq.matches);
  }, []);

  const readout = value
    ? `Selected: ${formatCoord(value.latitude)}, ${formatCoord(value.longitude)}`
    : "No location selected";

  const [hintVisible, setHintVisible] = useState(false);
  const hintTimerRef = useRef<ReturnType<typeof setTimeout> | null>(null);
  const isMac = useMemo(() => detectIsMac(), []);
  const hintKeyLabel = isMac ? "\u2318" : "Ctrl";

  const flashHint = useCallback(() => {
    setHintVisible(true);
    if (hintTimerRef.current) clearTimeout(hintTimerRef.current);
    hintTimerRef.current = setTimeout(() => {
      setHintVisible(false);
      hintTimerRef.current = null;
    }, ZOOM_HINT_FADE_MS);
  }, []);

  useEffect(() => {
    return () => {
      if (hintTimerRef.current) clearTimeout(hintTimerRef.current);
    };
  }, []);

  return (
    <div className={cn("flex flex-col gap-2", className)}>
      <div
        className={cn(
          "relative w-full rounded-md border border-border overflow-hidden",
          heightClassName,
        )}
        data-testid="location-picker-shell"
      >
        <MapContainer
          // Always mount at world overview; `RecenterOnValueChange`
          // nudges in to `FIRST_PICK_ZOOM` (continent-level) when the
          // controlled `value` is non-null so the operator always sees
          // surrounding context on initial render.
          center={DEFAULT_CENTER}
          zoom={DEFAULT_ZOOM}
          minZoom={1}
          worldCopyJump
          scrollWheelZoom={false}
          zoomAnimation={animate}
          markerZoomAnimation={animate}
          className="h-full w-full"
        >
          {/* TileLayer key remounts on theme change so the basemap tracks the app theme. */}
          <TileLayer
            key={theme}
            url={tileUrl}
            attribution={TILE_ATTRIBUTION}
            subdomains={TILE_SUBDOMAINS}
          />
          <ClickToPlace onPick={onChange} />
          <RecenterOnValueChange value={value} />
          <ModifierZoomController onHintNeeded={flashHint} />
          {value ? (
            <Marker
              position={[value.latitude, value.longitude]}
              draggable
              eventHandlers={{
                dragend: (event) => {
                  const target = event.target as {
                    getLatLng: () => { lat: number; lng: number };
                  };
                  const next = target.getLatLng();
                  onChange({ latitude: next.lat, longitude: next.lng });
                },
              }}
            />
          ) : null}
        </MapContainer>
        <div
          data-testid="zoom-hint"
          aria-hidden={!hintVisible}
          className={cn(
            "pointer-events-none absolute inset-0 z-[1000] flex items-center justify-center transition-opacity duration-200",
            hintVisible ? "opacity-100" : "opacity-0",
          )}
        >
          <div className="rounded-md bg-black/70 px-4 py-2 text-sm font-medium text-white shadow-lg">
            Hold <kbd className="rounded bg-white/20 px-1.5 py-0.5 font-mono">{hintKeyLabel}</kbd>{" "}
            to zoom
          </div>
        </div>
      </div>
      <div className="flex items-center justify-between gap-3">
        <p role="status" aria-live="polite" className="text-sm text-muted-foreground">
          {readout}
        </p>
        <Button
          type="button"
          size="sm"
          variant="outline"
          aria-label={clearLabel}
          disabled={!value}
          onClick={() => onChange(null)}
        >
          Clear
        </Button>
      </div>
    </div>
  );
}

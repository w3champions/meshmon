# History components

Building blocks for the `/history/pair` page. The page renders latency,
loss, and MTR traces for one `(source, destination)` pair drawn from the
`measurements` + `mtr_traces` tables.

## Files

| File | Responsibility |
|------|----------------|
| `HistoryPairFilters.tsx` | Sticky filter bar with source + destination pickers, protocol chips, and range toggle (24h / 7d / 30d / 90d / custom). |
| `PairChart.tsx` | Latency (line + min/max band) and loss (bars) recharts `ComposedChart`s sharing one X axis. |
| `MtrTracesList.tsx` | Collapsible list of MTR traces with inline `RouteTopology` on expand. Newest-first ordering. |
| `reshape.ts` | `HistoryMeasurement[]` → `ChartRow[]` reshape helper keyed on `measured_at`, plus `protocolsPresent`. |

## Reshape contract

`reshapeForChart` pivots one row per `(protocol, measured_at)` into one
row per `measured_at` bucket with flat per-protocol keys:

```
{ t, icmp_avg, icmp_min, icmp_max, icmp_range_delta, icmp_loss,
     tcp_avg,  tcp_min,  tcp_max,  tcp_range_delta,  tcp_loss,
     udp_avg,  udp_min,  udp_max,  udp_range_delta,  udp_loss }
```

`*_range_delta` is `max - min` and is only emitted when both bounds are
present. It powers the min/max band — recharts 3.8 does not accept a
`[min, max]` tuple `dataKey` on `<Area>`, so the chart stacks two
`<Area>` components sharing `stackId="<proto>_band"`: a transparent
baseline at `*_min` plus the delta on top. The stack renders the filled
envelope.

## T42 null tolerance

Destination rows whose catalogue row has been deleted arrive with
`display_name === destination_ip`, `city`/`country_code`/`asn` all
`null`. The picker renders these as `"<ip> — no metadata"` rather than
treating the state as a rendering bug.

## Result cap

`/api/history/measurements` asks the database for up to 5001 rows as a
truncation probe: a response at exactly 5000 means the full set fit
inside the cap, while 5001 means the underlying set is larger and the
view was clipped. The page displays the first 5000 and surfaces a
"showing most recent 5,000" status line on the 5001 signal so
operators know to narrow the window.

# meshmon Alert Rules

This directory contains VictoriaMetrics alert rule files for meshmon.  Rules
are loaded by `vmalert` at runtime and tested offline with `vmalert-tool
unittest`.

---

## Shipping

The rule file is baked into the `ghcr.io/w3champions/meshmon-vmalert`
image at `/etc/vmalert/rules/rules.yaml`. The OSS and overlay compose
files both reference the image directly; there is no runtime volume
mount. Downstream deployments override by building a thin image on
top of `meshmon-vmalert`, not by mounting a different file.

---

## Directory layout

```
deploy/alerts/
  README.md              — this file
  *.yaml                 — alert rule groups (one file per category)
  tests/                 — vmalert-tool unit-test files (*.yaml)
```

---

## Category taxonomy

Each alert rule belongs to exactly one category.  The category is carried in
the `category` label on every firing alert and is used by Alertmanager routing
trees and the frontend filter panel (`frontend/src/lib/alerts-filter.ts`).

| Rule group file        | Category     | Covers                                                        |
|------------------------|--------------|---------------------------------------------------------------|
| `meshmon.path_health`  | `loss`       | Sustained packet-loss above threshold on a monitored path     |
| `meshmon.path_latency` | `latency`    | RTT above threshold on a monitored path                       |
| `meshmon.path_latency` | `anomaly`    | Statistical RTT anomaly (z-score / sudden spike)              |
| `meshmon.path_latency` | `jitter`     | High RTT variance / jitter on a monitored path                |
| `meshmon.path_availability` | `unreachable` | Path has been fully unreachable for a sustained window   |
| `meshmon.route_changes` | `topology`  | BGP / routing-table churn detected by agents                  |
| `meshmon.agent_health` | `agent`      | Agent itself is down or not reporting metrics                  |

---

## Label contract

Every alert produced by these rules carries a consistent set of labels.
Downstream consumers (Alertmanager routes, frontend filter, runbooks) rely on
this contract.  Do not remove labels; additions are allowed.

| Label       | Type     | Present on             | Example value            | Notes                                         |
|-------------|----------|------------------------|--------------------------|-----------------------------------------------|
| `alertname` | string   | all alerts             | `PathHighLoss`           | Human-readable, unique within a group         |
| `severity`  | string   | all alerts             | `critical`, `warning`    | Drives Alertmanager receiver selection        |
| `category`  | string   | all alerts             | `loss`, `latency`        | See taxonomy table above                      |
| `source`    | string   | path alerts            | `node-fra1`              | Agent node originating the measurement        |
| `target`    | string   | path alerts            | `node-ams2`              | Remote node being probed                      |
| `protocol`  | string   | most path alerts       | `tcp`, `udp`, `icmp`     | Probe protocol; see exceptions below          |

### Label exceptions

- **`meshmon.path_availability` / `PathUnreachable`** — does NOT carry a
  `protocol` label.  Unreachability is declared when all protocols report
  failure simultaneously; attaching a single protocol label would be
  misleading.

- **`meshmon.agent_health`** alerts — do NOT carry `target` or `protocol`
  labels.  An agent-down alert is scoped to the reporting node (`source`)
  only; there is no remote target or probe protocol involved.

---

## Validation

**Validation:** `cargo test -p meshmon-service --test alerts_validation` (integration — requires Docker running). Hermetic rule-metric cross-check: `cargo test -p meshmon-service --test alert_metrics_contract`. End-to-end delivery smoke: `cargo e2e` (requires the bundled compose stack to be running).

---

## Editing workflow

Follow these steps whenever you add or modify an alert rule:

1. **Edit the rule file** (`deploy/alerts/<group>.yaml`).
2. **Update or add the corresponding unit test** (`deploy/alerts/tests/<group>.yaml`).
   Unit tests must cover the firing threshold, the recovery threshold, and
   at least one edge case (e.g., exactly at threshold).
3. **Run the validator** to catch syntax errors before committing:
   ```bash
   cargo test -p meshmon-service --test alerts_validation
   cargo test -p meshmon-service --test alert_metrics_contract
   ```
4. **If you changed a category** (the `category` label value):
   - Update the taxonomy table in this README.
   - Update the Alertmanager routing tree in `deploy/alertmanager/alertmanager.yml`.
   - Update the frontend filter list in `frontend/src/lib/alerts-filter.ts`.
5. **If you added a new `alertname`**:
   - Add a runbook entry (link from the `runbook_url` annotation in the rule).
6. Commit with a message of the form:
   `feat(alerts): add PathHighJitter rule for latency jitter detection`

---

## Overlay / custom rule overrides

Operators can inject additional rule files or override the bundled ones by
mounting a custom file into the vmalert container.  In
`deploy/docker-compose.yml`, add a volume entry under the `meshmon-vmalert`
service:

```yaml
volumes:
  - ./custom-rules/my-overrides.yaml:/etc/vmalert/rules/my-overrides.yaml:ro
```

Pass the additional file to vmalert with the `-rule` flag:

```yaml
command:
  - -rule=/etc/vmalert/rules/*.yaml
  - -rule=/etc/vmalert/rules/my-overrides.yaml
```

Rules in the custom file are additive.  To silence a bundled alert, add a
`mute_time_intervals` entry in `alertmanager.yml` rather than deleting the
rule from the bundled files — that avoids conflicts on future upgrades.

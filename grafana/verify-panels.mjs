#!/usr/bin/env node
// grafana/verify-panels.mjs — contract-drift guard.
//
// Enforces `panels.json` ⊆ `grafana/dashboards/<uid_key>.json`. Each dashboard
// key in panels.json must:
//   1. have a matching dashboards/<uid_key>.json file.
//   2. declare the same top-level `uid`.
//   3. contain panels whose numeric `id` matches every entry in `panels[<name>]`.
//   4. contain templating variables whose `name` matches every `variables[name]`.
//
// Exit 0 on success, 1 on contract drift (prints a line per mismatch).
//
// Non-goal: validating dashboards NOT referenced in panels.json. Operator-browse
// dashboards (overview, agent) can exist with any panel layout; only dashboards
// the frontend iframes need the contract enforced.

import { readFile } from "node:fs/promises";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";

const HERE = dirname(fileURLToPath(import.meta.url));
const PANELS_PATH = join(HERE, "panels.json");
const DASHBOARDS_DIR = join(HERE, "dashboards");

function fail(msg) {
  process.stderr.write(`::error ::${msg}\n`);
}

function collectPanelIds(dashboard) {
  const ids = new Set();
  const walk = (panels) => {
    if (!Array.isArray(panels)) return;
    for (const p of panels) {
      if (typeof p?.id === "number") ids.add(p.id);
      if (Array.isArray(p?.panels)) walk(p.panels);
    }
  };
  walk(dashboard.panels);
  return ids;
}

function collectVariableNames(dashboard) {
  const vars = new Set();
  const list = dashboard?.templating?.list;
  if (!Array.isArray(list)) return vars;
  for (const v of list) {
    if (typeof v?.name === "string") vars.add(v.name);
  }
  return vars;
}

async function readJson(path) {
  const raw = await readFile(path, "utf8");
  return JSON.parse(raw);
}

async function main() {
  const panels = await readJson(PANELS_PATH);
  let failures = 0;

  for (const [key, entry] of Object.entries(panels)) {
    const uidKey = entry.uid_key ?? key;
    const dashboardPath = join(DASHBOARDS_DIR, `${uidKey}.json`);
    let dashboard;
    try {
      dashboard = await readJson(dashboardPath);
    } catch (err) {
      fail(`${key}: ${dashboardPath} missing or invalid JSON (${err.message})`);
      failures += 1;
      continue;
    }

    if (dashboard.uid !== uidKey) {
      fail(`${key}: dashboard uid "${dashboard.uid}" !== panels.json uid_key "${uidKey}"`);
      failures += 1;
    }

    const actualIds = collectPanelIds(dashboard);
    for (const [name, id] of Object.entries(entry.panels ?? {})) {
      if (!actualIds.has(id)) {
        fail(
          `${key}.panels.${name}: id=${id} not found in ${uidKey}.json ` +
            `(actual ids: ${[...actualIds].sort((a, b) => a - b).join(", ")})`
        );
        failures += 1;
      }
    }

    const actualVars = collectVariableNames(dashboard);
    for (const name of entry.variables ?? []) {
      if (!actualVars.has(name)) {
        fail(
          `${key}.variables: "${name}" not declared in ${uidKey}.json templating ` +
            `(actual: ${[...actualVars].sort().join(", ") || "—"})`
        );
        failures += 1;
      }
    }
  }

  if (failures > 0) {
    process.stderr.write(`\n${failures} contract drift${failures === 1 ? "" : "s"} detected.\n`);
    process.exit(1);
  }
  process.stdout.write("OK — grafana/panels.json matches dashboards/\n");
}

main().catch((err) => {
  fail(`unexpected: ${err.stack ?? err.message}`);
  process.exit(1);
});

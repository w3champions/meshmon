import { describe, expect, it } from "vitest";
import { ALERTMANAGER_BASE, buildAlertmanagerUrl } from "./alertmanager-link";

describe("buildAlertmanagerUrl", () => {
  it("uses the same-origin /alertmanager base", () => {
    expect(ALERTMANAGER_BASE).toBe("/alertmanager");
  });

  it("encodes a single matcher", () => {
    expect(buildAlertmanagerUrl({ alertname: "PathPacketLoss" })).toBe(
      `/alertmanager/#/alerts?filter=${encodeURIComponent('{alertname="PathPacketLoss"}')}`,
    );
  });

  it("joins multiple matchers with commas", () => {
    const url = buildAlertmanagerUrl({
      alertname: "PathPacketLoss",
      source: "brazil-north",
      target: "paris-core",
    });
    expect(url).toBe(
      `/alertmanager/#/alerts?filter=${encodeURIComponent(
        '{alertname="PathPacketLoss",source="brazil-north",target="paris-core"}',
      )}`,
    );
  });

  it("strips empty / undefined label values", () => {
    const url = buildAlertmanagerUrl({
      alertname: "PathPacketLoss",
      source: "",
      target: undefined,
    });
    expect(url).toBe(
      `/alertmanager/#/alerts?filter=${encodeURIComponent('{alertname="PathPacketLoss"}')}`,
    );
  });

  it("escapes double quotes inside values", () => {
    const url = buildAlertmanagerUrl({
      alertname: 'weird"name',
    });
    expect(url).toBe(
      `/alertmanager/#/alerts?filter=${encodeURIComponent('{alertname="weird\\"name"}')}`,
    );
  });

  it("returns null when no labels remain after stripping", () => {
    expect(buildAlertmanagerUrl({ alertname: "" })).toBeNull();
  });
});

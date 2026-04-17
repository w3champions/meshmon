import { describe, expect, it } from "vitest";
import { buildAlertmanagerUrl } from "./alertmanager-link";

describe("buildAlertmanagerUrl", () => {
  it("encodes a single matcher", () => {
    expect(buildAlertmanagerUrl("https://am.example/", { alertname: "PathPacketLoss" })).toBe(
      `https://am.example/#/alerts?filter=${encodeURIComponent('{alertname="PathPacketLoss"}')}`,
    );
  });

  it("joins multiple matchers with commas", () => {
    const url = buildAlertmanagerUrl("https://am.example/", {
      alertname: "PathPacketLoss",
      source: "brazil-north",
      target: "paris-core",
    });
    expect(url).toBe(
      `https://am.example/#/alerts?filter=${encodeURIComponent(
        '{alertname="PathPacketLoss",source="brazil-north",target="paris-core"}',
      )}`,
    );
  });

  it("strips empty / undefined label values", () => {
    const url = buildAlertmanagerUrl("https://am.example/", {
      alertname: "PathPacketLoss",
      source: "",
      target: undefined,
    });
    expect(url).toBe(
      `https://am.example/#/alerts?filter=${encodeURIComponent('{alertname="PathPacketLoss"}')}`,
    );
  });

  it("escapes double quotes inside values", () => {
    const url = buildAlertmanagerUrl("https://am.example/", {
      alertname: 'weird"name',
    });
    expect(url).toBe(
      `https://am.example/#/alerts?filter=${encodeURIComponent('{alertname="weird\\"name"}')}`,
    );
  });

  it("normalises the trailing slash on base", () => {
    const a = buildAlertmanagerUrl("https://am.example", { alertname: "X" });
    const b = buildAlertmanagerUrl("https://am.example/", { alertname: "X" });
    expect(a).toBe(b);
  });

  it("returns null when no labels remain after stripping", () => {
    expect(buildAlertmanagerUrl("https://am.example/", { alertname: "" })).toBeNull();
  });
});

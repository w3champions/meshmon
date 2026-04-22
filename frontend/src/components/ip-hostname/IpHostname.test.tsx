import { act, render, screen } from "@testing-library/react";
import type { ReactNode } from "react";
import { afterEach, beforeEach, describe, expect, test, vi } from "vitest";
import { IpHostname } from "@/components/ip-hostname/IpHostname";
import {
  IpHostnameProvider,
  useIpHostnameContext,
} from "@/components/ip-hostname/IpHostnameProvider";

class MockEventSource {
  static instances: MockEventSource[] = [];
  listeners: Record<string, Array<(event: { data: string }) => void>> = {};

  constructor(public url: string) {
    MockEventSource.instances.push(this);
  }
  addEventListener(name: string, handler: (event: { data: string }) => void): void {
    const list = this.listeners[name] ?? [];
    list.push(handler);
    this.listeners[name] = list;
  }
  removeEventListener(name: string, handler: (event: { data: string }) => void): void {
    const list = this.listeners[name];
    if (!list) return;
    const idx = list.indexOf(handler);
    if (idx >= 0) list.splice(idx, 1);
  }
  emit(payload: unknown): void {
    for (const h of this.listeners.hostname_resolved ?? []) {
      h({ data: JSON.stringify(payload) });
    }
  }
  close(): void {}
}

beforeEach(() => {
  MockEventSource.instances = [];
  vi.stubGlobal("EventSource", MockEventSource);
});

afterEach(() => {
  vi.unstubAllGlobals();
  vi.restoreAllMocks();
});

/**
 * Convenience wrapper that seeds the provider map before rendering the
 * child tree. Used by render-site tests that want a specific hostname
 * already resolved on first paint.
 */
function Seeder({
  seed,
  children,
}: {
  seed: Array<{ ip: string; hostname?: string | null }>;
  children: ReactNode;
}) {
  const { seedFromResponse } = useIpHostnameContext();
  // Seed once synchronously inside the render so the first paint picks it up.
  if (seed.length > 0) seedFromResponse(seed);
  return <>{children}</>;
}

function renderWithSeed(ui: ReactNode, seed: Array<{ ip: string; hostname?: string | null }>) {
  return render(
    <IpHostnameProvider>
      <Seeder seed={seed}>{ui}</Seeder>
    </IpHostnameProvider>,
  );
}

describe("<IpHostname>", () => {
  test("renders `ip (hostname)` on a positive hit with sr-only announce text", () => {
    renderWithSeed(<IpHostname ip="203.0.113.10" />, [
      { ip: "203.0.113.10", hostname: "mail.example.com" },
    ]);
    // Visible (aria-hidden) half — the parenthesised hostname is rendered.
    expect(screen.getByText("(mail.example.com)")).toBeInTheDocument();
    // Screen-reader half — one combined phrase announced without the parens.
    expect(screen.getByText("203.0.113.10, hostname mail.example.com")).toBeInTheDocument();
  });

  test("renders the bare IP on a cold miss (map key absent)", () => {
    renderWithSeed(<IpHostname ip="203.0.113.11" />, []);
    const root = screen.getByText("203.0.113.11");
    expect(root).toBeInTheDocument();
    // No sr-only companion on the bare render — the plain IP text is the announced string.
    expect(screen.queryByText(/hostname/)).not.toBeInTheDocument();
  });

  test("renders the bare IP on a confirmed negative (map value null)", () => {
    renderWithSeed(<IpHostname ip="203.0.113.12" />, [{ ip: "203.0.113.12", hostname: null }]);
    expect(screen.getByText("203.0.113.12")).toBeInTheDocument();
    expect(screen.queryByText(/hostname/)).not.toBeInTheDocument();
  });

  test("updates when the SSE stream delivers a hostname after mount", () => {
    renderWithSeed(<IpHostname ip="203.0.113.13" />, []);
    // Pre-stream: bare IP only.
    expect(screen.getByText("203.0.113.13")).toBeInTheDocument();
    expect(screen.queryByText(/hostname/)).not.toBeInTheDocument();

    act(() => {
      MockEventSource.instances[0]?.emit({ ip: "203.0.113.13", hostname: "late.example.com" });
    });
    expect(screen.getByText("203.0.113.13, hostname late.example.com")).toBeInTheDocument();
    expect(screen.getByText("(late.example.com)")).toBeInTheDocument();
  });

  test("renders IPv6 literals without brackets", () => {
    renderWithSeed(<IpHostname ip="2001:db8::1" />, [
      { ip: "2001:db8::1", hostname: "v6.example.com" },
    ]);
    expect(screen.getByText("(v6.example.com)")).toBeInTheDocument();
    expect(screen.getByText("2001:db8::1, hostname v6.example.com")).toBeInTheDocument();
  });

  test("renders nothing when fallback=none and the hostname is unknown", () => {
    const { container } = renderWithSeed(<IpHostname ip="203.0.113.14" fallback="none" />, []);
    expect(container).toBeEmptyDOMElement();
  });
});

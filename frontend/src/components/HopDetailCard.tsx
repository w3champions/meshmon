import type { components } from "@/api/schema.gen";
import { Button } from "@/components/ui/button";
import { Card, CardContent, CardDescription, CardHeader, CardTitle } from "@/components/ui/card";

type HopJson = components["schemas"]["HopJson"];

interface HopDetailCardProps {
  hop: HopJson;
  onClose: () => void;
}

export function HopDetailCard({ hop, onClose }: HopDetailCardProps) {
  return (
    <Card className="max-w-sm">
      <CardHeader>
        <div className="flex items-start justify-between gap-2">
          <div>
            <CardTitle>Hop {hop.position}</CardTitle>
            <CardDescription>Click a different node to switch</CardDescription>
          </div>
          <Button variant="ghost" size="sm" onClick={onClose} aria-label="Close hop detail">
            ×
          </Button>
        </div>
      </CardHeader>
      <CardContent className="text-sm flex flex-col gap-2">
        <section>
          <h3 className="text-xs uppercase text-muted-foreground">Observed IPs</h3>
          <ul className="font-mono">
            {hop.observed_ips.map((ip) => (
              <li key={ip.ip}>
                {ip.ip} <span className="text-muted-foreground">×{ip.freq}</span>
              </li>
            ))}
          </ul>
        </section>
        <section>
          <span className="text-muted-foreground">Avg RTT: </span>
          {(hop.avg_rtt_micros / 1000).toFixed(2)} ms
          <span className="text-muted-foreground"> ± </span>
          {(hop.stddev_rtt_micros / 1000).toFixed(2)} ms
        </section>
        <section>
          <span className="text-muted-foreground">Loss: </span>
          {(hop.loss_pct * 100).toFixed(1)}%
        </section>
      </CardContent>
    </Card>
  );
}

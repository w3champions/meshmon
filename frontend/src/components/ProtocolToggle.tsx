import { ToggleGroup, ToggleGroupItem } from "@/components/ui/toggle-group";

interface ProtocolToggleProps {
  value: "icmp" | "udp" | "tcp";
  autoValue?: "icmp" | "udp" | "tcp";
  onChange: (next: "icmp" | "udp" | "tcp") => void;
  className?: string;
}

export function ProtocolToggle({ value, autoValue, onChange, className }: ProtocolToggleProps) {
  return (
    <ToggleGroup
      type="single"
      value={value}
      onValueChange={(v) => {
        if (v === "icmp" || v === "udp" || v === "tcp") onChange(v);
      }}
      className={className}
      aria-label="Protocol"
    >
      {(["icmp", "udp", "tcp"] as const).map((p) => (
        <ToggleGroupItem key={p} value={p}>
          <span className="uppercase">{p}</span>
          {autoValue === p && value !== p && (
            <span className="ml-1 text-[10px] text-muted-foreground">(auto)</span>
          )}
        </ToggleGroupItem>
      ))}
    </ToggleGroup>
  );
}

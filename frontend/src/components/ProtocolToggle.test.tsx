import "@testing-library/jest-dom/vitest";
import { render, screen } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { describe, expect, test, vi } from "vitest";
import { ProtocolToggle } from "@/components/ProtocolToggle";

describe("ProtocolToggle", () => {
  test("renders three toggle items with the current selection pressed", () => {
    render(<ProtocolToggle value="icmp" onChange={() => {}} />);
    const icmp = screen.getByRole("radio", { name: /icmp/i });
    const udp = screen.getByRole("radio", { name: /udp/i });
    const tcp = screen.getByRole("radio", { name: /tcp/i });
    expect(icmp).toHaveAttribute("aria-checked", "true");
    expect(udp).toHaveAttribute("aria-checked", "false");
    expect(tcp).toHaveAttribute("aria-checked", "false");
  });

  test("fires onChange with the newly selected protocol", async () => {
    const onChange = vi.fn();
    render(<ProtocolToggle value="icmp" onChange={onChange} />);
    const user = userEvent.setup();
    await user.click(screen.getByRole("radio", { name: /udp/i }));
    expect(onChange).toHaveBeenCalledWith("udp");
  });

  test("renders '(auto)' only on the unselected auto-pick item", () => {
    render(<ProtocolToggle value="tcp" autoValue="icmp" onChange={() => {}} />);
    expect(screen.getByText(/\(auto\)/i)).toBeInTheDocument();
    expect(screen.getByRole("radio", { name: /icmp/i })).toHaveTextContent(/\(auto\)/i);
    expect(screen.getByRole("radio", { name: /tcp/i })).not.toHaveTextContent(/\(auto\)/i);
  });

  test("does not render '(auto)' when auto matches the current selection", () => {
    render(<ProtocolToggle value="icmp" autoValue="icmp" onChange={() => {}} />);
    expect(screen.queryByText(/\(auto\)/i)).not.toBeInTheDocument();
  });
});

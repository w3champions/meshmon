import { render, screen } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { describe, expect, test, vi } from "vitest";

import { CountryPicker } from "@/components/catalogue/CountryPicker";

describe("CountryPicker", () => {
  test("renders the placeholder when no country is selected", () => {
    render(<CountryPicker value={null} onChange={() => {}} />);
    // Radix renders the placeholder inside the trigger; the role
    // "combobox" is assigned to the trigger button.
    const trigger = screen.getByRole("combobox");
    expect(trigger).toHaveTextContent(/select a country/i);
  });

  test("shows the current selection", () => {
    render(<CountryPicker value={{ code: "DE", name: "Germany" }} onChange={() => {}} />);
    const trigger = screen.getByRole("combobox");
    expect(trigger.textContent).toMatch(/Germany/);
  });

  test("emits {code, name} together when a country is picked", async () => {
    const handler = vi.fn();
    const user = userEvent.setup();
    render(<CountryPicker value={null} onChange={handler} />);

    await user.click(screen.getByRole("combobox"));
    const germany = await screen.findByRole("option", {
      name: /Germany \(DE\)/,
    });
    await user.click(germany);

    expect(handler).toHaveBeenCalledWith({ code: "DE", name: "Germany" });
  });

  test("emits null when the clear option is picked", async () => {
    const handler = vi.fn();
    const user = userEvent.setup();
    render(<CountryPicker value={{ code: "DE", name: "Germany" }} onChange={handler} />);

    await user.click(screen.getByRole("combobox"));
    const clear = await screen.findByRole("option", { name: /— \(none\)/ });
    await user.click(clear);

    expect(handler).toHaveBeenCalledWith(null);
  });
});

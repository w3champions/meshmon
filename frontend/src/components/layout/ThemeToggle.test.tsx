import { render, screen } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { beforeEach, describe, expect, it } from "vitest";
import { useUiStore } from "@/stores/ui";
import { ThemeToggle } from "./ThemeToggle";

describe("ThemeToggle", () => {
  beforeEach(() => {
    useUiStore.setState({ theme: "dark" });
    document.documentElement.classList.remove("light");
  });

  it("flips the `light` class and the store on click", async () => {
    const user = userEvent.setup();
    render(<ThemeToggle />);
    expect(document.documentElement.classList.contains("light")).toBe(false);

    await user.click(screen.getByRole("button", { name: /toggle theme/i }));

    expect(document.documentElement.classList.contains("light")).toBe(true);
    expect(useUiStore.getState().theme).toBe("light");

    await user.click(screen.getByRole("button", { name: /toggle theme/i }));
    expect(document.documentElement.classList.contains("light")).toBe(false);
    expect(useUiStore.getState().theme).toBe("dark");
  });
});

import { MobileNavSheet } from "./MobileNavSheet";
import { ThemeToggle } from "./ThemeToggle";
import { UserMenu } from "./UserMenu";

export function AppBar() {
  return (
    <header
      className="flex items-center justify-between border-b border-border px-2 md:px-4 h-14"
      data-app-chrome="true"
    >
      <div className="flex items-center gap-1">
        <MobileNavSheet />
        <span className="font-semibold px-2">meshmon</span>
      </div>
      <div className="flex items-center gap-1">
        <ThemeToggle />
        <UserMenu />
      </div>
    </header>
  );
}

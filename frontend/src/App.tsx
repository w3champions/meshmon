import type { ReactNode } from "react";

interface AppProps {
  children: ReactNode;
}

export default function App({ children }: AppProps) {
  return (
    <div className="min-h-full flex flex-col">
      <header className="border-b border-border px-6 py-4">
        <h1 className="text-xl font-semibold">meshmon</h1>
      </header>
      <main className="flex-1 p-6">{children}</main>
    </div>
  );
}

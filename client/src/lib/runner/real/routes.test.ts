import { test, expect } from "bun:test";
import { ROUTES } from "./backendPure";

// The SAME fixture the Go route test asserts against (go/internal/server/routes_test.go).
const pinPath = `${import.meta.dir}/../../../../../api/routes.txt`;

function parsePin(text: string): Record<string, string> {
  const pinned: Record<string, string> = {};
  const lines = text.split("\n");
  for (let i = 0; i < lines.length; i++) {
    const line = lines[i].trim();
    if (line === "" || line.startsWith("#")) continue;
    const parts = line.split("|");
    if (parts.length !== 3)
      throw new Error(`line ${i + 1}: want 3 fields: ${line}`);
    pinned[parts[0].trim()] = parts[1].trim();
  }
  if (Object.keys(pinned).length === 0) throw new Error("pin is empty");
  return pinned;
}

const pinned = parsePin(await Bun.file(pinPath).text());

test("ROUTES matches api/routes.txt", () => {
  expect({ ...ROUTES } as Record<string, string>).toEqual(pinned);
});

import type { LegalAbout } from "./types";

let cached: Promise<LegalAbout> | undefined;

function legalURL(): string {
  return new URL("legal/about.json", document.baseURI).toString();
}

async function fetchLegal(): Promise<LegalAbout> {
  const response = await fetch(legalURL(), {
    credentials: "same-origin",
    headers: { Accept: "application/json" },
  });
  if (!response.ok)
    throw new Error(`legal notices returned ${response.status}`);
  const data = (await response.json()) as LegalAbout;
  if (
    !data.project ||
    !data.sourceURL ||
    !data.licenseURL ||
    !data.noticesURL ||
    !Array.isArray(data.components)
  ) {
    throw new Error("legal notices were incomplete");
  }
  return data;
}

export function loadLegal(): Promise<LegalAbout> {
  return (cached ??= fetchLegal());
}

export function retryLegal(): Promise<LegalAbout> {
  cached = undefined;
  return loadLegal();
}

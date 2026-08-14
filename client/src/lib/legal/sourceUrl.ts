const RELEASE_VERSION = /^(?:v)?\d+\.\d+\.\d+(?:-(?:alpha|beta|rc)\.\d+)?$/;

/** Return the exact tagged source tree for releases, or the repository root. */
export function sourceUrl(repository: string, version: string): string {
  const base = repository.replace(/\/$/, "");
  if (!RELEASE_VERSION.test(version)) return base;
  return `${base}/tree/v${version.replace(/^v/, "")}`;
}

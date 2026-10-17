// Reads the shared contract fixtures in api/, which the Rust side loads too.
const api = new URL("../../../api/", import.meta.url);

export const json = (name: string): Promise<any> => Bun.file(new URL(name, api)).json();

/** The rows of a `a | b | c` pin file, comments and blank lines skipped. */
export async function rows(name: string): Promise<string[][]> {
  return (await Bun.file(new URL(name, api)).text())
    .split("\n")
    .filter((line) => line.trim() && !line.startsWith("#"))
    .map((line) => line.split("|").map((cell) => cell.trim()));
}

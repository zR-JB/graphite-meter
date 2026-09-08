import { HISTORY_DB } from "../src/lib/history/dbSchema";
import { expect, openApp, startTest, test, waitForCompletion } from "./webview";

test("the first result saves after the application server becomes unreachable", async ({
  page,
}) => {
  await page.addInitScript(() => {
    localStorage.setItem(
      "graphite-meter:v1",
      JSON.stringify({
        resultHistoryPreference: "enabled",
        config: {
          stages: {
            latency: false,
            download: true,
            upload: false,
            bidirectional: false,
          },
          skipLoadedLatencyWhenStageOff: true,
          duration: { warmupMs: 0, downloadMs: 1500 },
          adaptive: { enabled: false },
        },
      }),
    );
  });
  await openApp(page);
  await startTest(page);
  await expect(page.locator(".gauge-panel")).toHaveAttribute(
    "data-phase",
    "download",
  );
  await page.blockRequests("*");
  await waitForCompletion(page);
  await expect
    .poll(() =>
      page.evaluate(async (schema) => {
        const databases = await indexedDB.databases();
        if (!databases.some((database) => database.name === schema.name))
          return 0;
        return new Promise<number>((resolve, reject) => {
          const opening = indexedDB.open(schema.name, schema.version);
          opening.onerror = () => reject(opening.error);
          opening.onsuccess = () => {
            const db = opening.result;
            const count = db
              .transaction(schema.resultsStore)
              .objectStore(schema.resultsStore)
              .count();
            count.onerror = () => {
              db.close();
              reject(count.error);
            };
            count.onsuccess = () => {
              db.close();
              resolve(count.result);
            };
          };
        });
      }, HISTORY_DB),
    )
    .toBe(1);
});

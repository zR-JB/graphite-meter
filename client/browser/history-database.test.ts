import { HISTORY_DB } from "../src/lib/history/dbSchema";
import { expect, openApp, test } from "./webview";

type TestPage = Parameters<typeof openApp>[0];
async function databaseState(page: TestPage) {
  return page.evaluate(
    (schema) =>
      new Promise((resolve, reject) => {
        const opening = indexedDB.open(schema.name);
        opening.onerror = () => reject(opening.error);
        opening.onsuccess = () => {
          const db = opening.result;
          const tx = db.transaction(schema.resultsStore);
          const store = tx.objectStore(schema.resultsStore);
          const records = store.getAll();
          tx.onerror = () => reject(tx.error);
          tx.oncomplete = () => {
            resolve({
              version: db.version,
              stores: Array.from(db.objectStoreNames),
              indexes: Array.from(store.indexNames),
              keyPath: store.keyPath,
              records: records.result,
            });
            db.close();
          };
        };
      }),
    HISTORY_DB,
  );
}

test("fresh history creates the current database and reopens it", async ({
  page,
}) => {
  await openApp(page);
  await page.evaluate(() => {
    location.hash = "/history";
  });
  await expect(
    page.getByRole("heading", { name: "No saved results" }),
  ).toBeVisible();
  const expected = {
    version: HISTORY_DB.version,
    stores: [HISTORY_DB.metadataStore, HISTORY_DB.resultsStore],
    indexes: [HISTORY_DB.completedAtIndex],
    keyPath: HISTORY_DB.resultKeyPath,
    records: [],
  };
  expect(await databaseState(page)).toEqual(expected);
  await page.reload();
  await expect(
    page.getByRole("heading", { name: "No saved results" }),
  ).toBeVisible();
  expect(await databaseState(page)).toEqual(expected);
});

for (const version of [1, HISTORY_DB.version + 1]) {
  test(`history refuses database version ${version} without changing its data or schema`, async ({
    page,
  }) => {
    await page.route("**/seed-history.html", (route) =>
      route.fulfill({
        body: "<!doctype html><title>History database fixture</title>",
        headers: { "content-type": "text/html" },
      }),
    );
    await page.goto("/seed-history.html");
    await page.evaluate(
      (schema) =>
        new Promise<void>((resolve, reject) => {
          const deletion = indexedDB.deleteDatabase(schema.name);
          deletion.onsuccess = () => resolve();
          deletion.onerror = () => reject(deletion.error);
        }),
      HISTORY_DB,
    );
    await page.evaluate(
      ({ schema, version }) =>
        new Promise<void>((resolve, reject) => {
          const opening = indexedDB.open(schema.name, version);
          opening.onupgradeneeded = () => {
            opening.result
              .createObjectStore(schema.resultsStore, {
                keyPath: schema.resultKeyPath,
              })
              .put({ id: "preserved", original: { value: "saved data" } });
          };
          opening.onerror = () => reject(opening.error);
          opening.onsuccess = () => {
            opening.result.close();
            resolve();
          };
        }),
      { schema: HISTORY_DB, version },
    );
    const before = await databaseState(page);
    await openApp(page);
    await page.evaluate(() => {
      location.hash = "/history";
    });
    await expect(
      page.getByRole("heading", { name: "History is unavailable" }),
    ).toBeVisible();
    expect(await databaseState(page)).toEqual(before);
    await page.getByRole("button", { name: "Retry", exact: true }).click();
    await expect(
      page.getByRole("heading", { name: "History is unavailable" }),
    ).toBeVisible();
    expect(await databaseState(page)).toEqual(before);
  });
}

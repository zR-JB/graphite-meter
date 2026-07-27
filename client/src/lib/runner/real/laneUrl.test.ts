import { test, expect } from "bun:test";
import { laneUrl, sessionDownloadUrl, type LaneUrlSpec } from "./backendPure";

const fetchSpec: LaneUrlSpec = {
  dir: "down",
  base: "http://meter.test:7246",
  downloadPath: "/download",
  uploadPath: "/upload",
  cbSeed: "r42",
  bytes: 1024,
  chunkDownload: false,
};

test("a download lane carries its size and a per-lane cache buster", () => {
  expect(laneUrl(fetchSpec, 3)).toBe(
    "http://meter.test:7246/download?bytes=1024&cb=r42-3",
  );
});

// The chunked worker appends its own adaptive size, so a baked-in one would be
// overridden by a second bytes parameter.
test("a chunked download lane omits the size", () => {
  expect(laneUrl({ ...fetchSpec, chunkDownload: true }, 0)).toBe(
    "http://meter.test:7246/download?cb=r42-0",
  );
});

test("an upload lane carries the minted id only once it exists", () => {
  const up: LaneUrlSpec = { ...fetchSpec, dir: "up" };
  expect(laneUrl(up, 1)).toBe("http://meter.test:7246/upload?cb=r42-1");
  expect(laneUrl(up, 1, "gmu_a/b")).toBe(
    "http://meter.test:7246/upload?cb=r42-1&id=gmu_a%2Fb",
  );
});

// A session upload is one URL for the whole session, so it takes no lane index
// and no cache buster: the id is what the server keys the aggregate on.
test("a session upload is keyed by id, and datagram mode is a flag on it", () => {
  const session = {
    origin: "https://meter.test",
    uploadPath: "/wt/upload",
    downloadPath: "/wt/download",
    datagrams: false,
  };
  expect(laneUrl({ ...fetchSpec, dir: "up", session }, 7, "gmu_x")).toBe(
    "https://meter.test/wt/upload?id=gmu_x",
  );
  expect(
    laneUrl(
      { ...fetchSpec, dir: "up", session: { ...session, datagrams: true } },
      0,
      "gmu_x",
    ),
  ).toBe("https://meter.test/wt/upload?id=gmu_x&datagrams=1");

  expect(sessionDownloadUrl(session, 2048, 4)).toBe(
    "https://meter.test/wt/download?bytes=2048&streams=4",
  );
  expect(sessionDownloadUrl({ ...session, datagrams: true }, 2048, 4)).toBe(
    "https://meter.test/wt/download?bytes=2048&datagrams=1",
  );
});

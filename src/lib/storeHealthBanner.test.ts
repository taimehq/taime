import { describe, it, expect } from "vitest";
import { storeHealthBanner } from "./storeHealthBanner";

describe("storeHealthBanner", () => {
  it("is null before the handshake and when the store is healthy", () => {
    expect(storeHealthBanner(null)).toBeNull();
    expect(storeHealthBanner({ status: "ok", detail: null })).toBeNull();
  });

  it("unavailable: persistence-off banner carrying the store error", () => {
    const b = storeHealthBanner({ status: "unavailable", detail: "disk I/O error" });
    expect(b).not.toBeNull();
    expect(b!.off).toBe(true);
    expect(b!.label).toBe("Attribution not recording");
    expect(b!.title).toContain("disk I/O error");
    // The detail can be missing (e.g. open failed before an error string).
    expect(storeHealthBanner({ status: "unavailable", detail: null })!.title).toContain("unknown");
  });

  it("recovered: archive banner pointing at the moved-aside file", () => {
    const b = storeHealthBanner({
      status: "recovered",
      detail: "taime.sqlite.corrupt-1780000000-123",
    });
    expect(b).not.toBeNull();
    expect(b!.off).toBe(false);
    expect(b!.label).toBe("Store recovered — history archived");
    expect(b!.title).toContain("taime.sqlite.corrupt-1780000000-123");
    expect(storeHealthBanner({ status: "recovered", detail: null })!.title).toContain(
      "a .corrupt file",
    );
  });
});

import { describe, expect, it } from "vitest";
import { folderUrl, formatDate, formatGoes, formatSize } from "./format";

describe("formatSize", () => {
  it("rounds to a readable unit", () => {
    expect(formatSize(0)).toBe("0 B");
    expect(formatSize(999)).toBe("999 B");
    expect(formatSize(1500)).toBe("1.5 KB");
    expect(formatSize(840_000)).toBe("840 KB");
    expect(formatSize(1_200_000_000)).toBe("1.2 GB");
  });
});

describe("formatDate", () => {
  const now = new Date("2026-11-06T12:00:00Z");
  it("shows the time for today and the date otherwise", () => {
    expect(formatDate("2026-11-06T09:30:00Z", now)).toMatch(/\d/);
    expect(formatDate("2026-09-14T09:30:00Z", now)).not.toMatch(/2026/);
    expect(formatDate("2025-09-14T09:30:00Z", now)).toMatch(/2025/);
  });
});

describe("folderUrl", () => {
  it("maps tree paths to app URLs", () => {
    expect(folderUrl("/")).toBe("/");
    expect(folderUrl("/Documents/Taxes 2025")).toBe("/files/Documents/Taxes%202025");
  });
});

describe("formatGoes", () => {
  const now = new Date("2026-11-07T12:00:00Z");
  it("counts the days left", () => {
    expect(formatGoes("2026-11-01T12:00:00Z", 30, now)).toBe("in 24 days");
    expect(formatGoes("2026-11-04T12:00:00Z", 5, now)).toBe("in 2 days");
    expect(formatGoes("2026-11-07T00:00:00Z", 1, now)).toBe("within a day");
    expect(formatGoes("2026-10-01T12:00:00Z", 30, now)).toBe("within a day");
  });
  it("is empty when nothing goes", () => {
    expect(formatGoes("2026-11-01T12:00:00Z", 0, now)).toBe("");
  });
});

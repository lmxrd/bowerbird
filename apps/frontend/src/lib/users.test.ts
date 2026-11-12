import { describe, expect, it } from "vitest";
import {
  formatExpires,
  formatUsage,
  inviteLink,
  limitToGb,
  parseLimitGb,
  usagePercent,
} from "./users";

describe("formatUsage", () => {
  it("shows what is used, and of what limit", () => {
    expect(formatUsage(1_200_000_000, null)).toBe("1.2 GB");
    expect(formatUsage(1_200_000_000, 10_000_000_000)).toBe("1.2 GB of 10 GB");
    expect(formatUsage(0, undefined)).toBe("0 B");
  });
});

describe("usagePercent", () => {
  it("is none without a limit, and at most 100", () => {
    expect(usagePercent(5, null)).toBeUndefined();
    expect(usagePercent(5, 10)).toBe(50);
    expect(usagePercent(50, 10)).toBe(100);
    expect(usagePercent(0, 0)).toBe(100);
  });
});

describe("parseLimitGb", () => {
  it("takes GB, empty for none, and refuses the rest", () => {
    expect(parseLimitGb("10")).toBe(10_000_000_000);
    expect(parseLimitGb(" 2,5 ")).toBe(2_500_000_000);
    expect(parseLimitGb("")).toBeNull();
    expect(parseLimitGb("-1")).toBeUndefined();
    expect(parseLimitGb("ten")).toBeUndefined();
  });
  it("round-trips through the field", () => {
    expect(limitToGb(2_500_000_000)).toBe("2.5");
    expect(limitToGb(null)).toBe("");
    expect(parseLimitGb(limitToGb(10_000_000_000))).toBe(10_000_000_000);
  });
});

describe("formatExpires", () => {
  const now = new Date("2026-11-08T12:00:00Z");
  it("counts the days left", () => {
    expect(formatExpires("2026-11-15T12:00:00Z", now)).toBe("expires in 7 days");
    expect(formatExpires("2026-11-09T06:00:00Z", now)).toBe("expires within a day");
  });
});

describe("inviteLink", () => {
  it("is the invite page on this server", () => {
    expect(inviteLink("abc", "https://files.example.com")).toBe(
      "https://files.example.com/invite/abc",
    );
  });
});

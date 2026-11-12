import { describe, expect, it } from "vitest";
import { formatSchedule, generatePassword, parseSchedule } from "./backup";

describe("schedules", () => {
  it("read both forms and write them back", () => {
    expect(parseSchedule("daily 3:30")).toEqual({ kind: "daily", time: "03:30" });
    expect(parseSchedule("every 6h")).toEqual({ kind: "every", hours: "6" });
    expect(parseSchedule("weekly")).toEqual({ kind: "daily", time: "03:00" });
    expect(formatSchedule({ kind: "daily", time: "03:30" })).toBe("daily 03:30");
    expect(formatSchedule({ kind: "every", hours: "12" })).toBe("every 12h");
  });
});

describe("generatePassword", () => {
  it("is long and differs each time", () => {
    const a = generatePassword();
    expect(a).toHaveLength(32);
    expect(a).not.toBe(generatePassword());
  });
});

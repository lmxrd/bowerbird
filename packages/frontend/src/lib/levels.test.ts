import { describe, expect, it } from "vitest";
import type { LevelSetting, SettingSource } from "organizer-sdk";
import {
  amountText,
  choiceOf,
  formatAmount,
  levelChoices,
  levelWrite,
  lockedByName,
  parseAmount,
  rangeText,
  shownValue,
  sourceName,
  userNote,
} from "./levels";

const source = (level: string, team_name: string | null = null): SettingSource => ({
  level,
  team_id: team_name ? "t" : null,
  team_name,
});

const level = (extra: Partial<LevelSetting>): LevelSetting =>
  ({
    key: "retention.trash_days",
    kind: "integer",
    mode: null,
    value: null,
    min: null,
    max: null,
    above: { key: "retention.trash_days", value: 30, min: null, max: null, locked_by: null },
    ...extra,
  }) as unknown as LevelSetting;

describe("the level control", () => {
  it("offers a range for numbers only", () => {
    expect(levelChoices("integer").map((c) => c.value)).toEqual([
      "default",
      "locked",
      "range",
      "unset",
    ]);
    expect(levelChoices("bool").map((c) => c.value)).toEqual(["default", "locked", "unset"]);
  });
  it("keeps the value shown when it locks or gives a default", () => {
    const unset = level({});
    expect(choiceOf(unset)).toBe("unset");
    expect(shownValue(unset)).toBe(30);
    expect(levelWrite(unset, "locked")).toEqual({ mode: "locked", value: 30 });
    const own = level({ mode: "default", value: 14 as never });
    expect(shownValue(own)).toBe(14);
    expect(levelWrite(own, "locked")).toEqual({ mode: "locked", value: 14 });
    expect(levelWrite(own, "range")).toEqual({ mode: "range", min: 14, max: null });
    expect(levelWrite(level({ max: 60 }), "range")).toEqual({ mode: "range", min: null, max: 60 });
    expect(levelWrite(own, "unset")).toBeNull();
  });
});

describe("amounts", () => {
  it("are shown in their unit", () => {
    expect(formatAmount(7, "integer", "days")).toBe("7 days");
    expect(formatAmount(1, "integer", "days")).toBe("1 day");
    expect(formatAmount(0.8, "fraction")).toBe("80 %");
    expect(rangeText(7, null, "integer", "days")).toBe("7 days or more");
    expect(rangeText(null, 30, "integer", "days")).toBe("at most 30 days");
    expect(rangeText(7, 30, "integer", "days")).toBe("7 to 30 days");
    expect(rangeText(0.5, 0.9, "fraction")).toBe("50 to 90 %");
    expect(rangeText(null, undefined, "integer")).toBeUndefined();
  });
  it("are typed as numbers, a fraction as a percent", () => {
    expect(parseAmount("14", "integer")).toBe(14);
    expect(parseAmount("1.5", "integer")).toBeUndefined();
    expect(parseAmount("", "integer")).toBeNull();
    expect(parseAmount("x", "integer")).toBeUndefined();
    expect(parseAmount("80", "fraction")).toBe(0.8);
    expect(amountText(0.8, "fraction")).toBe("80");
    expect(amountText(null, "integer")).toBe("");
  });
});

describe("what users are told", () => {
  it("names who set it", () => {
    expect(sourceName(source("superadmin"))).toBe("your administrator");
    expect(sourceName(source("team", "Staff"))).toBe("Staff");
    expect(sourceName(source("env"))).toBe("the server's configuration");
    expect(lockedByName({ locked_by: source("team", "Staff") })).toBe("Staff");
    expect(lockedByName({ locked_by: null })).toBeUndefined();
  });
  it("says the range and whose default it is", () => {
    const base = { kind: "integer", min: null, max: null, locked_by: null };
    expect(userNote({ ...base, source: source("user") }, "days")).toBeUndefined();
    expect(userNote({ ...base, min: 7, source: source("user") }, "days")).toBe("7 days or more.");
    expect(userNote({ ...base, source: source("team", "Staff") }, "days")).toBe("Staff's default.");
    expect(userNote({ ...base, max: 30, source: source("superadmin") }, "days")).toBe(
      "At most 30 days. Your administrator's default.",
    );
    expect(
      userNote({ ...base, source: source("team", "Staff"), locked_by: source("team", "Staff") }),
    ).toBeUndefined();
  });
});

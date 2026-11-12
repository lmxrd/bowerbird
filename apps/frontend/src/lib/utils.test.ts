import { describe, expect, it } from "vitest";
import { cn } from "./utils";

describe("cn", () => {
  it("joins class names and drops the falsy ones", () => {
    const active = false;
    expect(cn("a", active && "b", undefined, "c")).toBe("a c");
  });

  it("lets the later Tailwind utility win a conflict", () => {
    expect(cn("px-2 text-sm", "px-4")).toBe("text-sm px-4");
  });
});

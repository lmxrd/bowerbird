import { describe, expect, it } from "vitest";
import { percent, statusLabel, waitingCount } from "./drop-box";

describe("drop box", () => {
  it("counts the items that wait for the user", () => {
    const items = [
      { status: "proposed" as const },
      { status: "analysing" as const },
      { status: "failed" as const },
      { status: "confirmed" as const },
      { status: "undone" as const },
    ];
    expect(waitingCount(items)).toBe(3);
    expect(waitingCount([])).toBe(0);
  });

  it("shows a confidence as a whole percent", () => {
    expect(percent(0.874)).toBe("87%");
    expect(percent(1)).toBe("100%");
    expect(percent(1.3)).toBe("100%");
  });

  it("labels every status", () => {
    expect(statusLabel("needs_manual")).toBe("Place by hand");
    expect(statusLabel("auto_accepted")).toBe("Accepted");
  });
});

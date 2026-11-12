import { describe, expect, it } from "vitest";
import { ssoError } from "./sso";

describe("ssoError", () => {
  it("has a message for each code, and one for codes it doesn't know", () => {
    expect(ssoError("linked")).toContain("linked");
    expect(ssoError("disabled")).toContain("disabled");
    expect(ssoError("invite")).toContain("invite");
    expect(ssoError("something-new")).toBe("Single sign-on failed. Try again.");
    expect(ssoError(undefined)).toBeUndefined();
  });
});

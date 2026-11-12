import { describe, expect, it } from "vitest";
import {
  isSuperadminUrl,
  oneAtATime,
  refusalOf,
  steppedUp,
  stepUpNeeded,
  totpCode,
} from "./step-up";

const now = Date.parse("2026-11-09T12:00:00.000Z");
const later = "2026-11-09T12:10:00.000Z";
const earlier = "2026-11-09T11:59:59.000Z";

describe("refusalOf", () => {
  it("reads the code of a step-up refusal, and nothing else", () => {
    expect(refusalOf({ error: "Confirm it's you", code: "step_up_required" })).toBe(
      "step_up_required",
    );
    expect(refusalOf({ error: "Add one", code: "second_factor_required" })).toBe(
      "second_factor_required",
    );
    expect(refusalOf({ error: "Forbidden" })).toBeNull();
    expect(refusalOf({ error: "x", code: "other" })).toBeNull();
    expect(refusalOf("Forbidden")).toBeNull();
    expect(refusalOf(null)).toBeNull();
  });
});

describe("steppedUp", () => {
  it("holds until the time the server gave", () => {
    expect(steppedUp({ until: later, totp: true, passkey: false }, now)).toBe(true);
    expect(steppedUp({ until: earlier, totp: true, passkey: false }, now)).toBe(false);
    expect(steppedUp({ until: null, totp: true, passkey: false }, now)).toBe(false);
    expect(steppedUp(null, now)).toBe(false);
  });
});

describe("stepUpNeeded", () => {
  it("asks to confirm, or to add a second factor when there is none", () => {
    expect(stepUpNeeded({ until: later, totp: false, passkey: true }, now)).toBeNull();
    expect(stepUpNeeded({ until: null, totp: true, passkey: false }, now)).toBe("step_up_required");
    expect(stepUpNeeded({ until: earlier, totp: false, passkey: true }, now)).toBe(
      "step_up_required",
    );
    expect(stepUpNeeded({ until: null, totp: false, passkey: false }, now)).toBe(
      "second_factor_required",
    );
  });
});

describe("totpCode", () => {
  it("takes six digits, with spaces", () => {
    expect(totpCode("123456")).toBe("123456");
    expect(totpCode(" 123 456 ")).toBe("123456");
    expect(totpCode("12345")).toBeNull();
    expect(totpCode("abcdef")).toBeNull();
    expect(totpCode("abcde-fghjk")).toBeNull();
  });
});

describe("isSuperadminUrl", () => {
  it("knows the superadmin's endpoints, by either path", () => {
    expect(isSuperadminUrl("http://localhost:5173/api/superadmin/users")).toBe(true);
    expect(isSuperadminUrl("/api/admin/users")).toBe(true);
    expect(isSuperadminUrl("http://localhost:5173/api/auth/session")).toBe(false);
    expect(isSuperadminUrl("/api/files/superadmin/x")).toBe(false);
  });
});

describe("oneAtATime", () => {
  it("shares the open question's answer, then asks again", async () => {
    let asked = 0;
    let answer: (value: boolean) => void = () => {};
    const ask = oneAtATime((question: string) => {
      asked += question ? 1 : 0;
      return new Promise<boolean>((resolve) => {
        answer = resolve;
      });
    });
    const first = ask("a");
    const second = ask("b");
    answer(true);
    expect(await first).toBe(true);
    expect(await second).toBe(true);
    expect(asked).toBe(1);
    const third = ask("c");
    answer(false);
    expect(await third).toBe(false);
    expect(asked).toBe(2);
  });
});

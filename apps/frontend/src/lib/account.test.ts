import { describe, expect, it } from "vitest";
import { afterNewAccount, allowedNext } from "./account";

describe("afterNewAccount", () => {
  it("goes to the app, or to the two-factor step first", () => {
    expect(afterNewAccount(undefined)).toBe("app");
    expect(afterNewAccount("")).toBe("app");
    expect(afterNewAccount({ totp_required: false, totp_setup_required: true })).toBe("totp-setup");
    expect(afterNewAccount({ totp_required: true, totp_setup_required: false })).toBe("totp");
  });
});

describe("allowedNext", () => {
  it("goes on to a page of this app, with its query", () => {
    const contacts = "/settings/contacts?token=abc&providerDomain=cloud.example.com";
    expect(allowedNext(contacts)).toBe(contacts);
    expect(allowedNext("/settings/devices/approve?redirect_uri=organizer%3A%2F%2Fauth")).toBe(
      "/settings/devices/approve?redirect_uri=organizer%3A%2F%2Fauth",
    );
    expect(allowedNext("/files/Café menu")).toBe("/files/Caf%C3%A9%20menu");
  });

  it("never leaves this origin", () => {
    for (const bad of [
      "https://evil.example.com/",
      "//evil.example.com/",
      "/\\evil.example.com/",
      "\\\\evil.example.com",
      "/\t/evil.example.com",
      "/\n/evil.example.com",
      "javascript:alert(1)",
      "settings",
      "",
      undefined,
      42,
    ]) {
      expect(allowedNext(bad), String(bad)).toBeUndefined();
    }
  });

  it("leaves out the top, where a sign-in goes anyway", () => {
    expect(allowedNext("/")).toBeUndefined();
  });
});

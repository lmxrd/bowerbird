import { describe, expect, it } from "vitest";
import type { Subscription } from "organizer-sdk/types";
import {
  browserName,
  browserState,
  deviceDetail,
  keyBytes,
  subscriptionBody,
  thisBrowser,
} from "./push";

const device = (over: Partial<Subscription>): Subscription => ({
  id: "1",
  kind: "web",
  name: "Firefox on Linux",
  endpoint: "https://push.example.com/a",
  created_at: "2026-11-01T10:00:00.000Z",
  last_notice_at: null,
  available: true,
  ...over,
});

describe("keyBytes", () => {
  it("reads base64url without padding", () => {
    expect([...keyBytes("-_8B")]).toEqual([0xfb, 0xff, 0x01]);
    expect([...keyBytes("AQ")]).toEqual([1]);
  });
});

describe("browserName", () => {
  it("names the browser and the system", () => {
    expect(
      browserName("Mozilla/5.0 (X11; Linux x86_64; rv:140.0) Gecko/20100101 Firefox/140.0"),
    ).toBe("Firefox on Linux");
    expect(
      browserName(
        "Mozilla/5.0 (Macintosh; Intel Mac OS X 15_0) AppleWebKit/605.1.15 (KHTML, like Gecko) Version/18.0 Safari/605.1.15",
      ),
    ).toBe("Safari on macOS");
    expect(
      browserName(
        "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/140.0 Safari/537.36 Edg/140.0",
      ),
    ).toBe("Edge on Windows");
    expect(
      browserName(
        "Mozilla/5.0 (Linux; Android 15) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/140.0 Mobile Safari/537.36",
      ),
    ).toBe("Chrome on Android");
    expect(browserName("curl/8")).toBe("A browser");
  });
});

describe("subscriptionBody", () => {
  it("sends the endpoint and the keys", () => {
    expect(
      subscriptionBody(
        { endpoint: "https://push.example.com/a", keys: { p256dh: "BK", auth: "AA" } },
        "Firefox",
      ),
    ).toEqual({
      kind: "web",
      name: "Firefox",
      endpoint: "https://push.example.com/a",
      keys: { p256dh: "BK", auth: "AA" },
    });
  });

  it("refuses a subscription without keys", () => {
    expect(() => subscriptionBody({ endpoint: "https://x" }, "Firefox")).toThrow();
  });
});

describe("thisBrowser", () => {
  it("finds the row by its endpoint", () => {
    const rows = [device({ id: "1" }), device({ id: "2", endpoint: "https://push.example.com/b" })];
    expect(thisBrowser(rows, "https://push.example.com/b")?.id).toBe("2");
    expect(thisBrowser(rows, "https://push.example.com/c")).toBeUndefined();
    expect(thisBrowser(rows, undefined)).toBeUndefined();
  });
});

describe("deviceDetail", () => {
  const now = new Date("2026-11-08T12:00:00Z");
  it("says when it last got a notice", () => {
    expect(deviceDetail(device({}), now)).toBe("No notice yet");
    expect(deviceDetail(device({ last_notice_at: "2026-11-08T11:00:00.000Z" }), now)).toMatch(
      /^Last notice /,
    );
  });

  it("says when the iPhone app cannot get any yet", () => {
    expect(deviceDetail(device({ kind: "apns", available: false }), now)).toBe(
      "Notices not available yet",
    );
  });
});

describe("browserState", () => {
  it("follows the support, the permission and the subscription", () => {
    expect(browserState(false, undefined, false)).toBe("unsupported");
    expect(browserState(true, "denied", true)).toBe("blocked");
    expect(browserState(true, "granted", true)).toBe("on");
    expect(browserState(true, "granted", false)).toBe("off");
    expect(browserState(true, "default", true)).toBe("off");
  });
});

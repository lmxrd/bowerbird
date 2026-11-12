import type { NewSubscription, Subscription } from "organizer-sdk/types";
import { formatDate } from "@/lib/format";

/** The service worker that shows the notices (`public/sw.js`). */
export const SERVICE_WORKER = "/sw.js";

/** A VAPID key as the Push API takes it: base64url to bytes. */
export function keyBytes(base64url: string): Uint8Array<ArrayBuffer> {
  const base64 = base64url.replace(/-/g, "+").replace(/_/g, "/");
  const padded = base64 + "=".repeat((4 - (base64.length % 4)) % 4);
  const binary = atob(padded);
  const bytes = new Uint8Array(new ArrayBuffer(binary.length));
  for (let i = 0; i < binary.length; i++) bytes[i] = binary.charCodeAt(i);
  return bytes;
}

/** A name for this browser in the list of devices: "Firefox on Linux". */
export function browserName(userAgent: string): string {
  const browser = /Edg\//.test(userAgent)
    ? "Edge"
    : /Firefox\//.test(userAgent)
      ? "Firefox"
      : /Chrome\//.test(userAgent)
        ? "Chrome"
        : /Safari\//.test(userAgent)
          ? "Safari"
          : "A browser";
  const system = /iPhone|iPad/.test(userAgent)
    ? "iOS"
    : /Android/.test(userAgent)
      ? "Android"
      : /Mac OS X/.test(userAgent)
        ? "macOS"
        : /Windows/.test(userAgent)
          ? "Windows"
          : /Linux|X11/.test(userAgent)
            ? "Linux"
            : undefined;
  return system ? `${browser} on ${system}` : browser;
}

/** What the server is sent for this browser's subscription (`toJSON()`). */
export function subscriptionBody(
  subscription: { endpoint?: string; keys?: Record<string, string> },
  name: string,
): NewSubscription {
  const { endpoint, keys } = subscription;
  if (!endpoint || !keys?.p256dh || !keys.auth) {
    throw new Error("The browser gave a subscription without its keys");
  }
  return { kind: "web", name, endpoint, keys: { p256dh: keys.p256dh, auth: keys.auth } };
}

/** This browser's row among the devices, by its endpoint. */
export function thisBrowser(
  subscriptions: readonly Subscription[],
  endpoint: string | undefined,
): Subscription | undefined {
  return endpoint ? subscriptions.find((s) => s.endpoint === endpoint) : undefined;
}

/** The line under a device's name. */
export function deviceDetail(device: Subscription, now: Date = new Date()): string {
  if (!device.available) return "Notices not available yet";
  return device.last_notice_at
    ? `Last notice ${formatDate(device.last_notice_at, now)}`
    : "No notice yet";
}

/** Where this browser stands. */
export type BrowserState = "unsupported" | "blocked" | "on" | "off";

/** Whether this browser gets notices, from what it says. */
export function browserState(
  supported: boolean,
  permission: NotificationPermission | undefined,
  subscribed: boolean,
): BrowserState {
  if (!supported) return "unsupported";
  if (permission === "denied") return "blocked";
  return subscribed && permission === "granted" ? "on" : "off";
}

/** The Push API is there (on iPhone, only once added to the home screen). */
export function pushSupported(): boolean {
  return (
    typeof window !== "undefined" &&
    "serviceWorker" in navigator &&
    "PushManager" in window &&
    "Notification" in window
  );
}

/** This browser's push subscription, if it has one. */
export async function currentSubscription(): Promise<PushSubscription | null> {
  const registration = await navigator.serviceWorker.getRegistration(SERVICE_WORKER);
  return (await registration?.pushManager.getSubscription()) ?? null;
}

/**
 * Ask for the permission, register the service worker and subscribe with
 * the server's key: what the server is then sent.
 */
export async function subscribe(vapidPublicKey: string): Promise<NewSubscription> {
  const permission = await Notification.requestPermission();
  if (permission !== "granted") {
    throw new Error(
      permission === "denied"
        ? "Notices are blocked for this site: allow them in the browser's settings"
        : "Notices were not allowed",
    );
  }
  const registration = await navigator.serviceWorker.register(SERVICE_WORKER);
  await navigator.serviceWorker.ready;
  const subscription = await registration.pushManager.subscribe({
    userVisibleOnly: true,
    applicationServerKey: keyBytes(vapidPublicKey),
  });
  return subscriptionBody(subscription.toJSON(), browserName(navigator.userAgent));
}

// Organizer's service worker: it shows the notices (docs/M6D_DESIGN.md).
// A push says nothing (its body is an encrypted "{}"): what it stands for
// is asked of this server, never carried through the push service. Plain
// JavaScript, copied as it is to the root of the build (a service worker
// must be served from the scope it controls).

const GENERIC = { title: "Organizer", body: "Something new for you", path: "/", kind: "news" };

/** The newest notice, or the generic words when the server cannot say. */
async function newest() {
  try {
    const response = await fetch("/api/notifications/news", { credentials: "same-origin" });
    if (!response.ok) return GENERIC;
    const news = await response.json();
    return news.notices[0] ?? null;
  } catch {
    return GENERIC;
  }
}

self.addEventListener("install", () => self.skipWaiting());
self.addEventListener("activate", (event) => event.waitUntil(self.clients.claim()));

self.addEventListener("push", (event) => {
  event.waitUntil(
    (async () => {
      const notice = await newest();
      // Nothing new (a wake-up already shown): a browser that asks for a
      // visible notice for every push still gets one.
      const shown = notice ?? GENERIC;
      await self.registration.showNotification(shown.title, {
        body: shown.body,
        // One notice per kind: a newer count replaces the older one.
        tag: shown.kind,
        data: { path: shown.path },
      });
    })(),
  );
});

self.addEventListener("notificationclick", (event) => {
  event.notification.close();
  const url = new URL(event.notification.data?.path ?? "/", self.location.origin).href;
  event.waitUntil(
    (async () => {
      const windows = await self.clients.matchAll({ type: "window", includeUncontrolled: true });
      const open = windows.find((w) => new URL(w.url).origin === self.location.origin);
      if (open) {
        await open.focus();
        // A page this worker does not control yet cannot be navigated.
        await open.navigate(url).catch(() => self.clients.openWindow(url));
      } else {
        await self.clients.openWindow(url);
      }
    })(),
  );
});

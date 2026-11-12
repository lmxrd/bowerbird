import { useEffect, useState } from "react";
import { useMutation, useQuery } from "@tanstack/react-query";
import { createFileRoute } from "@tanstack/react-router";
import { GlobeIcon, SmartphoneIcon, TabletSmartphoneIcon } from "lucide-react";
import { toast } from "sonner";
import type { DeviceKind, Notifications, Subscription } from "organizer-sdk/types";
import {
  addPushSubscriptionMutation,
  getNotificationsOptions,
  removePushSubscriptionMutation,
  updateNotificationSettingsMutation,
} from "organizer-sdk/react-query";
import { LevelSettingsCard, lockedByOf, useMySettings } from "@/components/levels";
import { ListRow } from "@/components/list-row";
import { SuperadminGate } from "@/components/step-up";
import { userNote } from "@/lib/levels";
import { SettingsCard, SwitchField } from "@/components/settings";
import { Button } from "@/components/ui/button";
import { FieldDescription, FieldGroup } from "@/components/ui/field";
import { errorMessage, queryClient } from "@/lib/api";
import {
  browserState,
  currentSubscription,
  deviceDetail,
  pushSupported,
  subscribe,
  thisBrowser,
} from "@/lib/push";

export const Route = createFileRoute("/_app/settings/notifications")({
  component: NotificationsPage,
});

function refresh() {
  return queryClient.invalidateQueries({ queryKey: [{ _id: "getNotifications" }] });
}

function NotificationsPage() {
  const { user } = Route.useRouteContext();
  const notifications = useQuery(getNotificationsOptions());
  // This browser's push endpoint, once read from its service worker.
  const [endpoint, setEndpoint] = useState<string>();
  useEffect(() => {
    if (!pushSupported()) return;
    void currentSubscription().then((s) => setEndpoint(s?.endpoint));
  }, []);
  return (
    <>
      <ThisBrowser data={notifications.data} endpoint={endpoint} onEndpoint={setEndpoint} />
      <NotifyMeAbout data={notifications.data} error={notifications.error} />
      <Devices data={notifications.data} endpoint={endpoint} />
      {user.is_superadmin && (
        <SuperadminGate lazy title="For everyone">
          <LevelSettingsCard
            scope={{ level: "superadmin" }}
            prefixes={["notifications."]}
            title="For everyone"
            description="What every user is notified about: a default they may change, or locked."
          />
        </SuperadminGate>
      )}
    </>
  );
}

/** Notices in this browser: its push subscription. */
function ThisBrowser({
  data,
  endpoint,
  onEndpoint,
}: {
  data?: Notifications;
  endpoint?: string;
  onEndpoint: (endpoint?: string) => void;
}) {
  const supported = pushSupported();
  const row = data ? thisBrowser(data.subscriptions, endpoint) : undefined;
  const state = browserState(
    supported,
    supported ? Notification.permission : undefined,
    row !== undefined,
  );
  const add = useMutation(addPushSubscriptionMutation());
  const remove = useMutation(removePushSubscriptionMutation());
  const turnOn = useMutation({
    mutationFn: async (vapidKey: string) => {
      const body = await subscribe(vapidKey);
      await add.mutateAsync({ body });
      return body.endpoint;
    },
    onSuccess: (endpoint) => {
      onEndpoint(endpoint);
      void refresh();
    },
    onError: (error) => toast.error(errorMessage(error)),
  });
  const turnOff = useMutation({
    mutationFn: async () => {
      const subscription = await currentSubscription();
      if (row) await remove.mutateAsync({ path: { id: row.id } });
      await subscription?.unsubscribe();
    },
    onSuccess: () => {
      onEndpoint(undefined);
      void refresh();
    },
    onError: (error) => toast.error(errorMessage(error)),
  });
  const busy = turnOn.isPending || turnOff.isPending;
  return (
    <SettingsCard
      title="This browser"
      description="Get notices here even when Organizer is not open."
      enabled={supported ? state === "on" : undefined}
      action={
        state === "on" ? (
          <Button variant="outline" disabled={busy} onClick={() => turnOff.mutate()}>
            Turn off
          </Button>
        ) : state === "off" && data ? (
          <Button disabled={busy} onClick={() => turnOn.mutate(data.vapid_public_key)}>
            Turn on
          </Button>
        ) : undefined
      }
    >
      <p className="text-sm text-muted-foreground">
        {state === "unsupported"
          ? "This browser cannot show notices. On an iPhone or iPad, add Organizer to the home screen first."
          : state === "blocked"
            ? "Notices are blocked for this site: allow them in the browser's settings, then turn them on here."
            : state === "on"
              ? "This browser gets a notice when something below happens."
              : "This browser gets no notices."}
      </p>
    </SettingsCard>
  );
}

/** The switches of what is notified. */
function NotifyMeAbout({ data, error }: { data?: Notifications; error: unknown }) {
  const { user } = Route.useRouteContext();
  const save = useMutation({
    ...updateNotificationSettingsMutation(),
    onSuccess: (settings) =>
      queryClient.setQueryData(getNotificationsOptions().queryKey, (old?: Notifications) =>
        old ? { ...old, settings } : old,
      ),
    onError: (error) => toast.error(errorMessage(error)),
  });
  const settings = data?.settings;
  // What the levels above leave each switch: locked, or whose default it is.
  const levels = useMySettings();
  const level = (key: string, description?: string) => {
    const setting = levels.get(`notifications.${key}`);
    const note = setting && userNote(setting);
    return {
      lockedBy: lockedByOf(setting),
      description: [description, note].filter(Boolean).join(" ") || undefined,
    };
  };
  return (
    <SettingsCard title="Notify me about" error={error}>
      {settings && (
        <FieldGroup>
          <SwitchField
            id="notify-review"
            label="Items waiting for review"
            {...level("review", "At most one notice a quarter hour, with the count.")}
            checked={settings.review}
            disabled={save.isPending}
            onChange={(review) => save.mutate({ body: { ...settings, review } })}
          />
          <SwitchField
            id="notify-shared"
            label="Something shared with me"
            {...level("shared")}
            checked={settings.shared}
            disabled={save.isPending}
            onChange={(shared) => save.mutate({ body: { ...settings, shared } })}
          />
          {user.is_superadmin && (
            <SwitchField
              id="notify-backup"
              label="A failed backup"
              {...level("backup", "Superadmins only.")}
              checked={settings.backup}
              disabled={save.isPending}
              onChange={(backup) => save.mutate({ body: { ...settings, backup } })}
            />
          )}
          <FieldDescription>
            The notice only says something is new; the details come from this server, never through
            the push service.
          </FieldDescription>
        </FieldGroup>
      )}
    </SettingsCard>
  );
}

const icons: Record<DeviceKind, React.ReactNode> = {
  web: <GlobeIcon className="size-4 text-muted-foreground" />,
  unifiedpush: <TabletSmartphoneIcon className="size-4 text-muted-foreground" />,
  apns: <SmartphoneIcon className="size-4 text-muted-foreground" />,
};

/** Every browser and phone that gets notices. */
function Devices({ data, endpoint }: { data?: Notifications; endpoint?: string }) {
  const remove = useMutation({
    ...removePushSubscriptionMutation(),
    onSuccess: () => {
      void refresh();
      toast.success("Removed");
    },
    onError: (error) => toast.error(errorMessage(error)),
  });
  if (!data) return null;
  return (
    <SettingsCard title="Devices" description="The browsers and phones that get your notices.">
      {data.subscriptions.length === 0 ? (
        <p className="text-sm text-muted-foreground">
          None yet: turn notices on in a browser, or sign in with the app on your phone.
        </p>
      ) : (
        <div className="divide-y">
          {data.subscriptions.map((device: Subscription) => (
            <ListRow
              key={device.id}
              icon={icons[device.kind]}
              title={device.endpoint === endpoint ? `${device.name} (this browser)` : device.name}
              detail={deviceDetail(device)}
            >
              <Button
                variant="outline"
                size="sm"
                disabled={remove.isPending}
                onClick={() => remove.mutate({ path: { id: device.id } })}
              >
                Remove
              </Button>
            </ListRow>
          ))}
        </div>
      )}
    </SettingsCard>
  );
}

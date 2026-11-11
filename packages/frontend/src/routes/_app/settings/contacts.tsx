import { useState } from "react";
import { useMutation, useQuery } from "@tanstack/react-query";
import { createFileRoute, useNavigate } from "@tanstack/react-router";
import { ClockIcon, InboxIcon, SendIcon, Trash2Icon, UserRoundXIcon } from "lucide-react";
import { toast } from "sonner";
import type { Contact, FederatedInvite } from "organizer-sdk";
import {
  acceptFederatedInviteMutation,
  createFederatedInviteMutation,
  listContactsOptions,
  listFederatedInvitesOptions,
  removeContactMutation,
  revokeFederatedInviteMutation,
} from "organizer-sdk/react-query";
import { ConfirmDialog } from "@/components/confirm-dialog";
import { CopyButton } from "@/components/copy-button";
import { FormDialog } from "@/components/form-dialog";
import { ListRow } from "@/components/list-row";
import { RecipientIcon } from "@/components/recipient-icon";
import { RowMenu } from "@/components/row-menu";
import { SettingsCard } from "@/components/settings";
import { Button } from "@/components/ui/button";
import {
  Dialog,
  DialogContent,
  DialogDescription,
  DialogFooter,
  DialogHeader,
  DialogTitle,
} from "@/components/ui/dialog";
import { Field, FieldDescription, FieldLabel } from "@/components/ui/field";
import { Input } from "@/components/ui/input";
import { errorMessage, queryClient } from "@/lib/api";
import { contactDetail, contactName, inviteDetail, inviteFromLink } from "@/lib/federation";
import { formatDay } from "@/lib/format";
import { formatExpires } from "@/lib/users";

type Search = { token?: string; providerDomain?: string };

/**
 * Settings → Contacts (docs/M7E_DESIGN.md §3): the user's contacts on
 * other servers, their invites, and accepting one. It is also OCM's
 * `inviteAcceptDialog`: `?token=…&providerDomain=…` opens the accept
 * dialog for that invite.
 */
export const Route = createFileRoute("/_app/settings/contacts")({
  validateSearch: (search: Record<string, unknown>): Search => ({
    token: typeof search.token === "string" ? search.token : undefined,
    providerDomain: typeof search.providerDomain === "string" ? search.providerDomain : undefined,
  }),
  component: Contacts,
});

function refresh() {
  for (const _id of ["listContacts", "listFederatedInvites"]) {
    void queryClient.invalidateQueries({ queryKey: [{ _id }] });
  }
}

type Dialog =
  | { kind: "invite"; invite: FederatedInvite }
  | { kind: "accept"; invite: string }
  | { kind: "remove"; contact: Contact }
  | { kind: "revoke"; invite: FederatedInvite };

function Contacts() {
  const search = Route.useSearch();
  const navigate = useNavigate();
  const contacts = useQuery(listContactsOptions());
  const invites = useQuery(listFederatedInvitesOptions());
  // An accept dialog's link (OCM §5.3) opens the dialog at once.
  const linked = inviteFromLink(search, window.location.origin);
  const [dialog, setDialog] = useState<Dialog | undefined>(
    linked ? { kind: "accept", invite: linked } : undefined,
  );
  const close = () => {
    setDialog(undefined);
    if (search.token || search.providerDomain) {
      void navigate({ to: "/settings/contacts", search: {}, replace: true });
    }
  };
  const failed = (error: unknown) => toast.error(errorMessage(error));
  const create = useMutation({
    ...createFederatedInviteMutation(),
    onSuccess: (invite) => {
      refresh();
      setDialog({ kind: "invite", invite });
    },
    onError: failed,
  });
  const remove = useMutation({
    ...removeContactMutation(),
    onSuccess: () => {
      refresh();
      toast.success("Removed");
    },
    onError: failed,
  });
  const revoke = useMutation({
    ...revokeFederatedInviteMutation(),
    onSuccess: () => {
      refresh();
      toast.success("Invite revoked");
    },
    onError: failed,
  });
  const list = contacts.data ?? [];
  const inviteList = invites.data ?? [];
  return (
    <>
      <SettingsCard
        title="Contacts on other servers"
        description="People on other servers you can share with. Invite someone, or accept their invite: you become each other's contacts."
        error={contacts.error}
        action={
          <>
            <Button
              size="sm"
              variant="outline"
              onClick={() => setDialog({ kind: "accept", invite: "" })}
            >
              <InboxIcon />
              Accept an invite
            </Button>
            <Button size="sm" disabled={create.isPending} onClick={() => create.mutate({})}>
              <SendIcon />
              Invite someone
            </Button>
          </>
        }
      >
        {contacts.data && list.length === 0 && inviteList.length === 0 ? (
          <p className="text-sm text-muted-foreground">
            No contacts yet. Invite someone on another server, or accept their invite.
          </p>
        ) : (
          <div className="divide-y">
            {list.map((contact) => (
              <ListRow
                key={contact.id}
                icon={<RecipientIcon kind="user" label={contactName(contact)} />}
                title={contactName(contact)}
                detail={contactDetail(contact)}
              >
                <RowMenu
                  label={`Actions for ${contactName(contact)}`}
                  actions={[
                    {
                      label: "Remove…",
                      icon: <UserRoundXIcon />,
                      destructive: true,
                      onClick: () => setDialog({ kind: "remove", contact }),
                    },
                  ]}
                />
              </ListRow>
            ))}
            {inviteList.map((invite) => (
              <ListRow
                key={invite.id}
                icon={
                  <span className="flex size-6 shrink-0 items-center justify-center">
                    <ClockIcon className="size-4 text-muted-foreground" />
                  </span>
                }
                title={`Invite made ${formatDay(invite.created_at)}`}
                detail={inviteDetail(invite)}
              >
                {invite.link && (
                  <CopyButton text={invite.link} label="Copy link" copied="Link copied" size="sm" />
                )}
                <RowMenu
                  label="Actions for the invite"
                  actions={[
                    {
                      label: invite.accepted_by ? "Remove" : "Revoke…",
                      icon: <Trash2Icon />,
                      destructive: !invite.accepted_by,
                      onClick: () =>
                        invite.accepted_by
                          ? revoke.mutate({ path: { id: invite.id } })
                          : setDialog({ kind: "revoke", invite }),
                    },
                  ]}
                />
              </ListRow>
            ))}
          </div>
        )}
      </SettingsCard>
      {dialog?.kind === "invite" && <InviteMadeDialog invite={dialog.invite} onClose={close} />}
      {dialog?.kind === "accept" && (
        <AcceptInviteDialog
          invite={dialog.invite}
          fromLink={search.providerDomain}
          onClose={close}
        />
      )}
      {dialog?.kind === "remove" && (
        <ConfirmDialog
          title={`Remove ${contactName(dialog.contact)}?`}
          description="What you shared with them stays shared. To share with them again, you may need to invite them again."
          action="Remove"
          onConfirm={() => remove.mutate({ path: { id: dialog.contact.id } })}
          onClose={close}
        />
      )}
      {dialog?.kind === "revoke" && (
        <ConfirmDialog
          title="Revoke this invite?"
          description="Its code and link stop working."
          action="Revoke"
          onConfirm={() => revoke.mutate({ path: { id: dialog.invite.id } })}
          onClose={close}
        />
      )}
    </>
  );
}

/** The invite just made: its code and link to send, each with Copy. */
function InviteMadeDialog({ invite, onClose }: { invite: FederatedInvite; onClose: () => void }) {
  return (
    <Dialog open onOpenChange={(open) => !open && onClose()}>
      <DialogContent>
        <DialogHeader>
          <DialogTitle>Invite someone on another server</DialogTitle>
          <DialogDescription>
            Send them the code to paste on their server, or the link if their server shows an accept
            page. Once they accept, you are each other's contacts. It{" "}
            {formatExpires(invite.expires_at)}.
          </DialogDescription>
        </DialogHeader>
        {invite.invite && (
          <Field>
            <FieldLabel htmlFor="invite-code">Code</FieldLabel>
            <div className="flex gap-2">
              <Input id="invite-code" readOnly value={invite.invite} className="font-mono" />
              <CopyButton text={invite.invite} copied="Code copied" />
            </div>
          </Field>
        )}
        {invite.link && (
          <Field>
            <FieldLabel htmlFor="invite-link">Link</FieldLabel>
            <div className="flex gap-2">
              <Input id="invite-link" readOnly value={invite.link} />
              <CopyButton text={invite.link} copied="Link copied" />
            </div>
          </Field>
        )}
        <DialogFooter>
          <Button onClick={onClose}>Done</Button>
        </DialogFooter>
      </DialogContent>
    </Dialog>
  );
}

/**
 * "Accept an invite": the code (or link) someone on another server sent.
 * From an accept dialog's link, the invite is filled in, and says whose
 * server it is from.
 */
function AcceptInviteDialog({
  invite,
  fromLink,
  onClose,
}: {
  invite: string;
  /** The server of the invite, when it came by a link. */
  fromLink?: string;
  onClose: () => void;
}) {
  const [text, setText] = useState(invite);
  const ours = fromLink?.toLowerCase() === window.location.host.toLowerCase();
  const accept = useMutation({
    ...acceptFederatedInviteMutation(),
    onSuccess: (contact) => {
      refresh();
      toast.success(`${contactName(contact)} is a contact now`);
      onClose();
    },
  });
  return (
    <FormDialog
      title="Accept an invite"
      description={
        ours
          ? "This invite is from this server: send its link to the person on another server, who accepts it there."
          : fromLink
            ? `An invite from someone on ${fromLink}. Accept it to become each other's contacts.`
            : "Paste the code or link someone on another server sent you. You become each other's contacts."
      }
      action="Accept"
      pending={accept.isPending || ours}
      error={accept.error}
      onSubmit={() => accept.mutate({ body: { invite: text.trim() } })}
      onClose={onClose}
    >
      <Field>
        <FieldLabel htmlFor="accept-invite">Invite</FieldLabel>
        <Input
          id="accept-invite"
          value={text}
          required
          autoFocus
          onChange={(e) => setText(e.target.value)}
        />
        <FieldDescription>The code, or the link with its token.</FieldDescription>
      </Field>
    </FormDialog>
  );
}

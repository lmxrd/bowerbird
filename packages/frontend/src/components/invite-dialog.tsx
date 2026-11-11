import { useState } from "react";
import { useMutation } from "@tanstack/react-query";
import type { Invite } from "organizer-sdk";
import { createInviteMutation } from "organizer-sdk/react-query";
import { CopyButton } from "@/components/copy-button";
import { FormDialog } from "@/components/form-dialog";
import { SwitchField } from "@/components/settings";
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
import { queryClient } from "@/lib/api";
import { inviteLink } from "@/lib/users";

/**
 * "Invite someone": an email (or anyone with the link) and whether they are
 * a superadmin, then the link to send them, with Copy. Organizer sends nothing.
 */
export function InviteDialog({ onClose }: { onClose: () => void }) {
  const [email, setEmail] = useState("");
  const [superadmin, setSuperadmin] = useState(false);
  const [created, setCreated] = useState<Invite>();
  const create = useMutation({
    ...createInviteMutation(),
    onSuccess: (invite) => {
      setCreated(invite);
      void queryClient.invalidateQueries({ queryKey: [{ _id: "listInvites" }] });
    },
  });
  if (created) {
    const link = inviteLink(created.token);
    return (
      <Dialog open onOpenChange={(open) => !open && onClose()}>
        <DialogContent>
          <DialogHeader>
            <DialogTitle>Send this link</DialogTitle>
            <DialogDescription>
              {created.email ? `For ${created.email}: ` : "For anyone who has it: "}
              it works once, for 7 days.
            </DialogDescription>
          </DialogHeader>
          <code className="rounded-md bg-muted px-2 py-2 font-mono text-sm break-all">{link}</code>
          <DialogFooter>
            <CopyButton text={link} label="Copy link" copied="Link copied" />
            <Button onClick={onClose}>Done</Button>
          </DialogFooter>
        </DialogContent>
      </Dialog>
    );
  }
  return (
    <FormDialog
      title="Invite someone"
      description="You get a link to send them yourself."
      action="Create link"
      pending={create.isPending}
      error={create.error}
      onSubmit={() =>
        create.mutate({ body: { email: email.trim() || null, is_superadmin: superadmin } })
      }
      onClose={onClose}
    >
      <Field>
        <FieldLabel htmlFor="invite-email">Email (optional)</FieldLabel>
        <Input
          id="invite-email"
          type="email"
          placeholder="name@example.com"
          value={email}
          onChange={(e) => setEmail(e.target.value)}
          autoFocus
        />
        <FieldDescription>
          Only this email can use it. Empty: anyone with the link.
        </FieldDescription>
      </Field>
      <SwitchField
        id="invite-superadmin"
        label="As a superadmin"
        description="They can manage users and the settings of the whole server."
        checked={superadmin}
        onChange={setSuperadmin}
      />
    </FormDialog>
  );
}

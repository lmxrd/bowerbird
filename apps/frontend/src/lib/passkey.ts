/**
 * The browser's side of a passkey ceremony: the server's options (its
 * `{ publicKey }` JSON) in, the credential's JSON out, through WebAuthn's
 * own JSON helpers (WebAuthn level 3: every current browser).
 */
type Options = { publicKey: Record<string, unknown> };

function supported(): boolean {
  return (
    typeof PublicKeyCredential !== "undefined" &&
    "parseCreationOptionsFromJSON" in PublicKeyCredential &&
    "parseRequestOptionsFromJSON" in PublicKeyCredential
  );
}

/** Whether this browser can use passkeys at all. */
export const passkeysSupported = supported;

/** Make a passkey for the options of `/auth/passkeys/register`. */
export async function createPasskey(options: unknown): Promise<Record<string, unknown>> {
  if (!supported()) throw new Error("This browser can't use passkeys.");
  const publicKey = PublicKeyCredential.parseCreationOptionsFromJSON(
    (options as Options).publicKey as unknown as PublicKeyCredentialCreationOptionsJSON,
  );
  const credential = (await navigator.credentials.create({ publicKey })) as PublicKeyCredential;
  return credential.toJSON() as unknown as Record<string, unknown>;
}

/** Sign with a passkey for the options of `/auth/login/passkey/start`. */
export async function getPasskey(options: unknown): Promise<Record<string, unknown>> {
  if (!supported()) throw new Error("This browser can't use passkeys.");
  const publicKey = PublicKeyCredential.parseRequestOptionsFromJSON(
    (options as Options).publicKey as unknown as PublicKeyCredentialRequestOptionsJSON,
  );
  const credential = (await navigator.credentials.get({ publicKey })) as PublicKeyCredential;
  return credential.toJSON() as unknown as Record<string, unknown>;
}

/** A cancelled browser prompt is no error worth showing. */
export function cancelled(error: unknown): boolean {
  return error instanceof DOMException && error.name === "NotAllowedError";
}

/**
 * What the login page says for each single sign-on error code. The codes
 * are the backend's `Failure` (packages/backend/src/oidc.rs): change both.
 */
export const SSO_ERRORS: Record<string, string> = {
  unavailable:
    "Single sign-on isn't available right now. Try again in a moment, or sign in with your password.",
  state: "That sign-in took too long or was started in another browser. Try again.",
  provider: "The sign-in provider refused or couldn't finish the sign-in. Try again.",
  token: "The sign-in provider's answer couldn't be verified. Try again, or ask your superadmin.",
  email: "Your account at the provider has no verified email. Ask your superadmin.",
  unknown_user: "No account here matches your email. Ask your superadmin.",
  linked: "This account is linked to another one at the provider. Ask your superadmin.",
  disabled: "This account is disabled. Ask your superadmin.",
  invite: "That invite link is used, expired, revoked or for another email. Ask for a new one.",
  misconfigured:
    "Single sign-on is set up wrongly (the callback URL's scheme). Ask your superadmin.",
};

export function ssoError(code: string | undefined): string | undefined {
  if (!code) return undefined;
  return SSO_ERRORS[code] ?? "Single sign-on failed. Try again.";
}

import type { CreatedInvitation } from '../api/types';

export function organizationInvitationUrl(origin: string, invitation: CreatedInvitation): string {
  const url = new URL('/auth/setup', origin);
  if (invitation.setup_token) url.searchParams.set('setup_token', invitation.setup_token);
  return url.toString();
}

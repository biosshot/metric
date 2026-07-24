import type { CreatedInvitation } from '../api/types';

export function organizationInvitationUrl(origin: string, invitation: CreatedInvitation): string {
  const url = new URL('/auth/setup', origin);
  url.searchParams.set('setup_token', invitation.setup_token);
  url.searchParams.set('organization_id', invitation.organization_id);
  return url.toString();
}

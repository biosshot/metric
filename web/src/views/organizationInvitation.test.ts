import { describe, expect, it } from 'vitest';
import { organizationInvitationUrl } from './organizationInvitation';

describe('organization invitation URL', () => {
  it('targets the dedicated public password-setup route', () => {
    expect(
      organizationInvitationUrl('http://localhost:4001', {
        setup_token: 'a'.repeat(64),
        organization_id: '7315819048328739377',
        existing_account: false,
      }),
    ).toBe(`http://localhost:4001/auth/setup?setup_token=${'a'.repeat(64)}`);
  });
});

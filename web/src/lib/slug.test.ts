import { describe, expect, it } from 'vitest';
import { suggestedSlug } from './slug';

describe('suggestedSlug', () => {
  it('creates a stable lowercase slug from a display name', () => {
    expect(suggestedSlug(' Payments API v2 ')).toBe('payments-api-v2');
  });

  it('transliterates Cyrillic names instead of producing an empty slug', () => {
    expect(suggestedSlug('Платёжный сервис')).toBe('platezhnyy-servis');
  });

  it('does not leave a trailing separator after applying the length limit', () => {
    expect(suggestedSlug('long project name', 13)).toBe('long-project');
  });
});

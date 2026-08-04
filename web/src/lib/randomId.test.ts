import { describe, expect, it } from 'vitest';
import { randomHexId } from './randomId';

describe('randomHexId', () => {
  it('creates a lowercase 128-bit identifier without randomUUID', () => {
    expect(randomHexId()).toMatch(/^[0-9a-f]{32}$/);
  });
});

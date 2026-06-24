import { describe, it, expect } from 'vitest';
import { VENUES, venueSortKey } from './panels';

describe('VENUES catalogue', () => {
  it('lists every venue the scanner trades', () => {
    const ids = VENUES.map((v) => v.id).sort();
    expect(ids).toEqual(['bybit', 'cantex', 'kucoin', 'oneswap', 'silvana', 'temple']);
  });

  it('puts Silvana first (it is the flagship venue across the UI)', () => {
    expect(VENUES[0]!.id).toBe('silvana');
  });

  it('venueSortKey lifts silvana above any other id', () => {
    const ids = ['bybit', 'silvana', 'kucoin', 'cantex'];
    const sorted = [...ids].sort((a, b) => venueSortKey(a) - venueSortKey(b));
    expect(sorted[0]).toBe('silvana');
  });

  it('every venue has a brand colour and either a glyph or a monogram', () => {
    for (const v of VENUES) {
      expect(v.color).toMatch(/^#[0-9A-Fa-f]{6}$/);
      expect(Boolean(v.glyph) || Boolean(v.mono)).toBe(true);
    }
  });

  it('glyph paths point at /venues/*.svg', () => {
    for (const v of VENUES) {
      if (v.glyph) expect(v.glyph).toMatch(/^\/venues\/.+\.svg$/);
    }
  });
});

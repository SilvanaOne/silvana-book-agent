import { describe, it, expect } from 'vitest';
import { DEMO_COIN_SHARE, TOKEN_COLORS, UI_TOKENS } from './tokens';

describe('UI token catalogue', () => {
  it('lists CC, CBTC, USDC, CETH, USDCx', () => {
    expect([...UI_TOKENS]).toEqual(['CC', 'CBTC', 'USDC', 'CETH', 'USDCx']);
  });

  it('defines a colour for every UI token', () => {
    for (const t of UI_TOKENS) expect(TOKEN_COLORS[t]).toMatch(/^#/);
  });

  it('demo coin share covers all UI tokens', () => {
    expect(DEMO_COIN_SHARE.map((d) => d.label).sort()).toEqual([...UI_TOKENS].sort());
  });
});

import { describe, it, expect } from 'vitest';
import { render, screen, within } from '@testing-library/react';
import { buildSeries, SpreadChart } from './SpreadChart';
import type { SpreadDto } from '@/lib/api';
import { CHART_PAIR_KEYS } from '@/lib/demo';

function s(id: string, basePairKey: string, spreadBps: number, tsMs: number): SpreadDto {
  return {
    id,
    ts: new Date(tsMs).toISOString(),
    basePairKey,
    buyVenueId: 'oneswap',
    sellVenueId: 'temple',
    buyPrice: '0.02',
    sellPrice: '0.0205',
    spreadBps,
    estProfitUsd: '2',
    acted: false,
  };
}

describe('buildSeries', () => {
  it('filters by pair, sorts by time, and scales into the viewbox', () => {
    const rows = [
      s('1', 'CC/USDCx', 100, 3000),
      s('2', 'CC/USDC', 999, 1000), // different pair — excluded
      s('3', 'CC/USDCx', 300, 1000),
      s('4', 'CC/USDCx', 200, 2000),
    ];
    const pts = buildSeries(rows, 'CC/USDCx');
    expect(pts).toHaveLength(3);
    // sorted ascending by ts → bps order 300, 200, 100
    expect(pts.map((p) => p.bps)).toEqual([300, 200, 100]);
    // x is monotonically increasing
    expect(pts[0]!.x).toBeLessThan(pts[1]!.x);
    expect(pts[1]!.x).toBeLessThan(pts[2]!.x);
    // highest bps maps to the smallest y (top of chart)
    expect(pts[0]!.y).toBeLessThan(pts[2]!.y);
  });

  it('returns empty for an unknown pair', () => {
    expect(buildSeries([s('1', 'CC/USDCx', 100, 1000)], 'NOPE')).toEqual([]);
  });
});

describe('<SpreadChart>', () => {
  it('always uses fixed demo chart data and shows the DEMO badge', () => {
    render(<SpreadChart spreads={[s('1', 'CC/USDCx', 100, 1000)]} />);
    expect(screen.getByRole('img', { name: /spread chart/i })).toBeInTheDocument();
    expect(screen.getByText(/demo/i)).toBeInTheDocument();
  });

  it('lists fixed chart pairs including CBTC and CETH even with many live ticks', () => {
    const rows = Array.from({ length: 20 }, (_, i) =>
      s(String(i), 'CC/USDCx', i % 2 === 0 ? 312 : 50, 1000 + i * 1000),
    );
    render(<SpreadChart spreads={rows} />);
    const select = screen.getByRole('combobox', { name: /pair/i });
    const options = within(select)
      .getAllByRole('option')
      .map((o) => o.textContent);
    expect(options).toEqual([...CHART_PAIR_KEYS]);
    expect(options).toContain('CC/CBTC');
    expect(options).toContain('CETH/USDC');
  });

  it('renders smooth demo series for the selected pair (not live zig-zag)', () => {
    const rows = [
      s('1', 'CC/USDCx', 312, 1000),
      s('2', 'CC/USDCx', 50, 2000),
      s('3', 'CC/USDCx', 312, 3000),
      s('4', 'CC/USDCx', 50, 4000),
    ];
    render(<SpreadChart spreads={rows} />);
    expect(screen.getAllByText('215 bps').length).toBeGreaterThanOrEqual(1);
    expect(screen.queryByText('312 bps')).toBeNull();
    expect(screen.queryByText('50 bps')).toBeNull();
  });
});

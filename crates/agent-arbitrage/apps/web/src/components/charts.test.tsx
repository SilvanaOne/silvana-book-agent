import { describe, it, expect } from 'vitest';
import { render, screen } from '@testing-library/react';
import { buildSegments, donutPath, polar, HBarChart, Histogram, Donut, type Datum } from './charts';

describe('buildSegments', () => {
  it('splits values into angle ranges summing to 360°', () => {
    const segs = buildSegments([
      { label: 'a', value: 30 },
      { label: 'b', value: 10 },
    ]);
    expect(segs).toHaveLength(2);
    expect(segs[0]!.start).toBe(0);
    expect(segs[0]!.end).toBeCloseTo(270, 5); // 30/40 of 360
    expect(segs[1]!.end).toBeCloseTo(360, 5);
    expect(segs[0]!.pct).toBeCloseTo(0.75, 5);
  });

  it('drops zero/empty values and returns [] for all-zero', () => {
    expect(buildSegments([{ label: 'a', value: 0 }])).toEqual([]);
    const segs = buildSegments([
      { label: 'a', value: 5 },
      { label: 'b', value: 0 },
    ]);
    expect(segs.map((s) => s.label)).toEqual(['a']);
  });
});

describe('polar + donutPath', () => {
  it('polar 0° is straight up from centre', () => {
    const [x, y] = polar(60, 60, 50, 0);
    expect(x).toBeCloseTo(60, 5);
    expect(y).toBeCloseTo(10, 5);
  });
  it('donutPath emits a closed arc path', () => {
    const d = donutPath(60, 60, 54, 33, 0, 90);
    expect(d.startsWith('M ')).toBe(true);
    expect(d.trim().endsWith('Z')).toBe(true);
    expect(d).toContain('A 54 54');
    expect(d).toContain('A 33 33');
  });
});

describe('chart components render', () => {
  const venue: Datum[] = [
    { label: 'Bybit', value: 8, color: '#F7A600' },
    { label: 'Silvana Book', value: 3, color: '#D6448F' },
  ];
  const sizes: Datum[] = [
    { label: '<50', value: 2 },
    { label: '500+', value: 5 },
  ];

  it('HBarChart shows labels + values', () => {
    render(<HBarChart data={venue} />);
    expect(screen.getByText('Bybit')).toBeInTheDocument();
    expect(screen.getByText('8')).toBeInTheDocument();
  });

  it('HBarChart shows empty state with no data', () => {
    render(<HBarChart data={[]} />);
    expect(screen.getByText(/no data yet/i)).toBeInTheDocument();
  });

  it('Histogram renders all buckets', () => {
    render(<Histogram data={sizes} />);
    expect(screen.getByText('<50')).toBeInTheDocument();
    expect(screen.getByText('500+')).toBeInTheDocument();
  });

  it('Donut renders the total and legend', () => {
    render(<Donut data={venue} centerLabel="spreads" />);
    expect(screen.getByText('11')).toBeInTheDocument(); // total 8+3
    expect(screen.getByText('spreads')).toBeInTheDocument();
  });
});

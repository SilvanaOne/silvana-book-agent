import { describe, it, expect, vi, beforeEach } from 'vitest';
import { render, screen, fireEvent } from '@testing-library/react';
import { InfoTip } from './InfoTip';

beforeEach(() => {
  // jsdom doesn't lay out geometry; stub getBoundingClientRect on the icon
  // so the position calculator has predictable inputs.
  Object.defineProperty(window, 'innerWidth', { configurable: true, value: 1000 });
  Object.defineProperty(window, 'innerHeight', { configurable: true, value: 800 });
});

function withIconRect(rect: Partial<DOMRect>) {
  const orig = HTMLElement.prototype.getBoundingClientRect;
  HTMLElement.prototype.getBoundingClientRect = function () {
    if ((this as HTMLElement).tagName === 'BUTTON') {
      return {
        x: 0, y: 0, top: 0, left: 0, right: 0, bottom: 0, width: 0, height: 0,
        toJSON: () => ({}),
        ...rect,
      } as DOMRect;
    }
    return orig.apply(this);
  };
  return () => {
    HTMLElement.prototype.getBoundingClientRect = orig;
  };
}

describe('<InfoTip>', () => {
  it('renders the icon button with the text as a title fallback', () => {
    render(<InfoTip text="Hover me" label="info" />);
    const btn = screen.getByRole('button', { name: 'info' });
    expect(btn).toHaveAttribute('title', 'Hover me');
  });

  it('shows the bubble in a body portal on hover, clamped inside the viewport', () => {
    const restore = withIconRect({
      // icon at the far-right edge of a 1000px viewport
      left: 985, right: 1000, top: 200, bottom: 215, width: 15, height: 15,
    });
    render(<InfoTip text="Bubble text" label="info" />);
    expect(screen.queryByRole('tooltip')).toBeNull();
    fireEvent.mouseEnter(screen.getByRole('button', { name: 'info' }));

    const bubble = screen.getByRole('tooltip');
    expect(bubble).toHaveTextContent('Bubble text');
    // portaled to <body>, not inside the InfoTip <span>
    expect(bubble.parentElement).toBe(document.body);

    const left = parseFloat(bubble.style.left);
    const width = parseFloat(bubble.style.width);
    // bubble must be inside the viewport with a margin >= 8 px (we use 12)
    expect(left).toBeGreaterThanOrEqual(8);
    expect(left + width).toBeLessThanOrEqual(1000 - 8);
    restore();
  });

  it('hides the bubble on mouseleave', () => {
    const restore = withIconRect({ left: 500, right: 515, top: 200, bottom: 215, width: 15, height: 15 });
    render(<InfoTip text="Bubble text" />);
    fireEvent.mouseEnter(screen.getByRole('button'));
    expect(screen.getByRole('tooltip')).toBeInTheDocument();
    fireEvent.mouseLeave(screen.getByRole('button'));
    expect(screen.queryByRole('tooltip')).toBeNull();
    restore();
  });

  it('shows on keyboard focus and hides on blur', () => {
    const restore = withIconRect({ left: 500, right: 515, top: 200, bottom: 215, width: 15, height: 15 });
    render(<InfoTip text="Bubble text" />);
    fireEvent.focus(screen.getByRole('button'));
    expect(screen.getByRole('tooltip')).toBeInTheDocument();
    fireEvent.blur(screen.getByRole('button'));
    expect(screen.queryByRole('tooltip')).toBeNull();
    restore();
  });

  it('clamps width to (viewport − margins) on a tiny viewport', () => {
    Object.defineProperty(window, 'innerWidth', { configurable: true, value: 320 });
    const restore = withIconRect({ left: 10, right: 25, top: 100, bottom: 115, width: 15, height: 15 });
    render(<InfoTip text="x" />);
    fireEvent.mouseEnter(screen.getByRole('button'));
    const bubble = screen.getByRole('tooltip');
    const width = parseFloat(bubble.style.width);
    expect(width).toBeLessThanOrEqual(320 - 24);
    restore();
  });

  it('flips below when the icon is near the top of the viewport', () => {
    const restore = withIconRect({ left: 500, right: 515, top: 0, bottom: 15, width: 15, height: 15 });
    render(<InfoTip text="Bubble text" />);
    fireEvent.mouseEnter(screen.getByRole('button'));
    const top = parseFloat(screen.getByRole('tooltip').style.top);
    // bottom of icon was 15 → bubble would sit at 15 + GAP = 23 (below icon)
    expect(top).toBeGreaterThanOrEqual(15);
    restore();
  });
});

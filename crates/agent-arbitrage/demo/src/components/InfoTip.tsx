'use client';

import {
  useCallback,
  useEffect,
  useId,
  useLayoutEffect,
  useRef,
  useState,
  type ReactNode,
} from 'react';
import { createPortal } from 'react-dom';

/**
 * "(i)" info tooltip — keyboard-accessible, rendered into a portal at the
 * document root so it can never be clipped by an overflow:hidden ancestor,
 * and computes its own position so the bubble never spills outside the
 * viewport. (The previous pure-CSS version overflowed when an icon was near
 * the right edge of the page.)
 *
 * Placement strategy:
 *   - horizontally: try to centre the bubble on the icon, then clamp to a
 *     12 px margin on either side of the viewport.
 *   - vertically: prefer above the icon; flip below if there isn't enough
 *     room.
 *   - width: capped at 260 px, but never exceeds (viewport − margins).
 *
 * Re-measures on scroll + resize while the bubble is open.
 */
export type InfoTipProps = Readonly<{
  text: string;
  label?: string;
  // kept for backwards compatibility with earlier callers — placement and
  // alignment are now auto-computed and these props are ignored.
  placement?: 'top' | 'bottom';
  align?: 'center' | 'start' | 'end';
  children?: ReactNode;
}>;

interface TipPos {
  readonly top: number;
  readonly left: number;
  readonly width: number;
}

const MARGIN = 12; // viewport gutter
const MAX_W = 280; // bubble cap
const GAP = 8; // distance between icon and bubble
const APPROX_H = 64; // approx bubble height used for first-frame placement

export function InfoTip({ text, label, children }: InfoTipProps) {
  const btnRef = useRef<HTMLButtonElement>(null);
  const bubbleRef = useRef<HTMLDivElement>(null);
  const [pos, setPos] = useState<TipPos | null>(null);
  const bubbleId = useId();
  const open = pos !== null;

  const compute = useCallback(() => {
    const btn = btnRef.current;
    if (!btn || typeof window === 'undefined') return;
    const rect = btn.getBoundingClientRect();
    const vw = window.innerWidth;
    const vh = window.innerHeight;
    const width = Math.min(MAX_W, vw - MARGIN * 2);

    // horizontal: centre on icon, then clamp to viewport gutters
    const centre = rect.left + rect.width / 2;
    let left = centre - width / 2;
    if (left < MARGIN) left = MARGIN;
    if (left + width > vw - MARGIN) left = vw - MARGIN - width;

    // vertical: prefer above; if measured bubble exists, use its real height
    const bubbleH = bubbleRef.current?.offsetHeight ?? APPROX_H;
    let top = rect.top - GAP - bubbleH;
    if (top < MARGIN) top = Math.min(rect.bottom + GAP, vh - MARGIN - bubbleH);

    setPos({ top, left, width });
  }, []);

  const show = useCallback(() => compute(), [compute]);
  const hide = useCallback(() => setPos(null), []);

  // re-measure after first mount with real bubble height + on scroll/resize
  useLayoutEffect(() => {
    if (!open) return;
    compute();
  }, [open, compute]);

  useEffect(() => {
    if (!open) return;
    const update = () => compute();
    window.addEventListener('scroll', update, true);
    window.addEventListener('resize', update);
    return () => {
      window.removeEventListener('scroll', update, true);
      window.removeEventListener('resize', update);
    };
  }, [open, compute]);

  return (
    <span className="infotip">
      {children}
      <button
        ref={btnRef}
        type="button"
        className="infotip__icon"
        aria-label={label ?? 'More info'}
        aria-describedby={open ? bubbleId : undefined}
        title={text}
        onMouseEnter={show}
        onMouseLeave={hide}
        onFocus={show}
        onBlur={hide}
      >
        <span aria-hidden>i</span>
      </button>
      {open && typeof document !== 'undefined'
        ? createPortal(
            <div
              ref={bubbleRef}
              id={bubbleId}
              role="tooltip"
              className="infotip__bubble"
              style={{
                position: 'fixed',
                top: pos!.top,
                left: pos!.left,
                width: pos!.width,
                maxWidth: 'none',
                transform: 'none',
                right: 'auto',
                bottom: 'auto',
                opacity: 1,
                pointerEvents: 'none',
              }}
            >
              {text}
            </div>,
            document.body,
          )
        : null}
    </span>
  );
}

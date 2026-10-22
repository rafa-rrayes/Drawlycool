import { useLayoutEffect, useMemo, useRef, useState, type HTMLAttributes, type ReactNode } from "react";
import rough from "roughjs";

const generator = rough.generator();

/** Stable 31-bit seed from a string so a box keeps its wobble across renders. */
export function seedFrom(value: string) {
  let h = 2166136261;
  for (let i = 0; i < value.length; i++) {
    h ^= value.charCodeAt(i);
    h = Math.imul(h, 16777619);
  }
  return (h >>> 0) % 2 ** 31 || 1;
}

/** Same rounded-rectangle path Excalidraw uses for "round" rectangles. */
function roundedRectPath(w: number, h: number, r: number) {
  return (
    `M ${r} 0 L ${w - r} 0 Q ${w} 0, ${w} ${r} L ${w} ${h - r} ` +
    `Q ${w} ${h}, ${w - r} ${h} L ${r} ${h} Q 0 ${h}, 0 ${h - r} L 0 ${r} Q 0 0, ${r} 0`
  );
}

type Props = Omit<HTMLAttributes<HTMLDivElement>, "children"> & {
  seed: number;
  radius?: number;
  strokeWidth?: number;
  dashed?: boolean;
  className?: string;
  children?: ReactNode;
};

/**
 * A block whose border is drawn with rough.js, matching the hand-drawn look of
 * an Excalidraw rectangle.
 */
export function RoughBox({ seed, radius = 24, strokeWidth = 1.5, dashed, className, children, ...rest }: Props) {
  const ref = useRef<HTMLDivElement>(null);
  const [size, setSize] = useState<{ w: number; h: number } | null>(null);

  useLayoutEffect(() => {
    const el = ref.current;
    if (!el) return;
    const observer = new ResizeObserver(([entry]) => {
      // Border box, not contentRect: the outline must include the padding.
      const box = entry.borderBoxSize[0];
      const w = Math.round(box.inlineSize);
      const h = Math.round(box.blockSize);
      setSize((prev) => (prev && prev.w === w && prev.h === h ? prev : { w, h }));
    });
    observer.observe(el, { box: "border-box" });
    return () => observer.disconnect();
  }, []);

  const paths = useMemo(() => {
    if (!size || size.w < 4 || size.h < 4) return [];
    const r = Math.min(radius, size.w / 4, size.h / 4);
    const drawable = generator.path(roundedRectPath(size.w, size.h, r), {
      seed,
      roughness: 1,
      bowing: 1,
      stroke: "currentColor",
      strokeWidth,
      preserveVertices: true,
      disableMultiStroke: dashed,
      strokeLineDash: dashed ? [8, 9] : undefined,
    });
    return generator.toPaths(drawable);
  }, [size, seed, radius, strokeWidth, dashed]);

  return (
    <div ref={ref} className={`rough-box ${className ?? ""}`} {...rest}>
      <svg className="rough-box__outline" aria-hidden width="100%" height="100%">
        {paths.map((p, i) => (
          <path
            key={i}
            d={p.d}
            stroke={p.stroke}
            strokeWidth={p.strokeWidth}
            strokeDasharray={dashed ? "8 9" : undefined}
            fill="none"
            strokeLinecap="round"
          />
        ))}
      </svg>
      {children}
    </div>
  );
}

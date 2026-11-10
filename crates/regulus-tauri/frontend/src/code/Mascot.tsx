// The Regulus Code mascot: a small stellar lion drawn from the shared pixel grid
// (assets/design/regulus-code-mascot.md). Each role is painted with the theme token the grid names; the
// only motion is the change of pose, in steps, and none with reduced motion. It is never
// the only sign of a state: an icon and a word always go with it.
import { useMemo } from "react";
import { t } from "@/lib/i18n";
import { mascotDrawing, tokenColor, type MascotMood, type MascotSize } from "./mascot";

export function Mascot({ size = "full", mood = "idle", scale = 2, decorative = false }: {
  size?: MascotSize;
  mood?: MascotMood;
  /** Screen pixels per grid cell: a whole number keeps the edges crisp. */
  scale?: number;
  /** Next to text that already names it: hidden from assistive tech. */
  decorative?: boolean;
}) {
  const drawing = useMemo(() => mascotDrawing(size, mood), [size, mood]);
  const label = decorative ? { "aria-hidden": true } : { role: "img", "aria-label": t("code.mascot") };
  return <svg className="mascot" data-mood={mood} data-poses={drawing.eyes.length} viewBox={`0 0 ${drawing.width} ${drawing.height}`}
    width={drawing.width * scale} height={drawing.height * scale} shapeRendering="crispEdges" {...label}>
    {drawing.body.map((run) => <rect key={`${run.x},${run.y}`} style={{ fill: tokenColor(run.token) }} x={run.x} y={run.y} width={run.width} height={1} />)}
    {drawing.eyes.map((eyes, pose) => <g key={pose} className="mascot-eyes">
      {eyes.map((eye) => <rect key={`${eye.x},${eye.y}`} style={{ fill: tokenColor(eye.token) }} x={eye.x} y={eye.y} width={1} height={1} />)}
    </g>)}
  </svg>;
}

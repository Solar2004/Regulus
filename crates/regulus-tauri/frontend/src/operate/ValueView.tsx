import { useState } from "react";
import { t } from "@/lib/i18n";
import { renderValue } from "./logic";
import { useOperateSettings } from "./settings";

/** A bus value, compact: text as text, JSON on one line; the rest on demand. */
export function ValueView({ value }: { value: unknown }) {
  const max = useOperateSettings((s) => s.maxValueLength);
  const [open, setOpen] = useState(false);
  const shown = renderValue(value, max);
  return <span className="value-view">
    {open ? <pre>{shown.full}</pre> : <code>{shown.short}</code>}
    {shown.expandable && <button type="button" className="link-button" onClick={() => setOpen(!open)}>{t(open ? "operate.value.less" : shown.json && !shown.truncated ? "operate.value.expand" : "operate.value.more")}</button>}
  </span>;
}

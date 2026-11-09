import { useState, type FormEvent } from "react";
import { Send } from "lucide-react";
import { Button } from "@/components/ui/button";
import { t } from "@/lib/i18n";

/** A text box that sends on Enter (Shift+Enter: new line); cleared when `onSend` says it went. */
export function Composer({ label, busy, disabled, onSend }: { label: string; busy?: boolean; disabled?: boolean; onSend: (text: string) => Promise<boolean> }) {
  const [text, setText] = useState("");
  const submit = async (event: FormEvent) => {
    event.preventDefault();
    const value = text.trim();
    if (!value || busy || disabled) return;
    if (await onSend(value)) setText("");
  };
  return <form className="operate-composer" onSubmit={submit}>
    <textarea aria-label={label} placeholder={label} value={text} rows={2} disabled={disabled} onChange={(e) => setText(e.target.value)}
      onKeyDown={(e) => { if (e.key === "Enter" && !e.shiftKey) { e.preventDefault(); void submit(e); } }} />
    <Button type="submit" size="sm" disabled={busy || disabled || !text.trim()}><Send size={14} />{t("operate.send")}</Button>
  </form>;
}


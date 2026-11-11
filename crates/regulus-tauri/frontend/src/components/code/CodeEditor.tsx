import { useEffect, useRef, useState, type ReactNode } from "react";
import { t } from "@/lib/i18n";
import { registerFlush } from "./flush";
import type { EditorHandle } from "./setup";
import type { PlacedProblem } from "./syntax";
import "./code-editor.css";

let loader: Promise<typeof import("./setup")> | null = null;

/** The editor's code, fetched once and shared; a failed fetch is retried next time. */
export function loadEditor() {
  loader ??= import("./setup").catch((error: unknown) => { loader = null; throw error; });
  return loader;
}

/** Height of one line of code, as the editor's theme sets it. */
const LINE = "1.45em";
const lines = (count: number) => `calc(${count} * ${LINE} + 2 * var(--rg-space-2))`;

export interface CodeEditorProps {
  /** The text as its owner has it; a different value from outside replaces what is shown. */
  value: string;
  language?: string;
  /** What Tab inserts: `"\t"` or spaces. */
  indent: string;
  label: string;
  /** Writes the text to its owner: after `commitDelayMs` without typing, on blur, and on `flushEdits()`. */
  onCommit: (text: string) => void;
  commitDelayMs: number;
  /** Milliseconds after a change before parser problems show. */
  lintDelayMs: number;
  /** The first change not yet committed, so the owner can say there is something to save. */
  onPending?: () => void;
  /** Mod-s inside the editor; without it the key goes on to the page. */
  onSave?: () => void;
  readOnly?: boolean;
  /** Problems from the core's check, by line and column. */
  problems?: PlacedProblem[];
  /** Grows from `minLines` to `maxLines`, then scrolls; without them it fills its parent. */
  minLines?: number;
  maxLines?: number;
  className?: string;
  /** Shown if the editor's code cannot be fetched: a plain control that still edits. */
  fallback: () => ReactNode;
  /** Receives the editor once it is open (and `null` when it closes). */
  onReady?: (handle: EditorHandle | null) => void;
  /** Whether `flushEdits()` (profile save, check, run) writes this editor's text; a file with its own Save says no. */
  flushWithProfile?: boolean;
}

/**
 * A code editor whose text lives in the editor while typing: a keystroke does not touch
 * the owner's document, so nothing else re-renders. The owner gets the text when typing
 * pauses, when the editor loses focus, or when something asks every editor to flush.
 */
export function CodeEditor(props: CodeEditorProps) {
  const host = useRef<HTMLDivElement>(null);
  const handle = useRef<EditorHandle | null>(null);
  const latest = useRef(props);
  latest.current = props;
  const committed = useRef(props.value);
  const pending = useRef(false);
  const timer = useRef<ReturnType<typeof setTimeout> | null>(null);
  const [state, setState] = useState<"loading" | "ready" | "failed">("loading");

  const commit = () => {
    if (timer.current) { clearTimeout(timer.current); timer.current = null; }
    if (!pending.current || !handle.current) return;
    pending.current = false;
    const text = handle.current.text();
    if (text === committed.current) return;
    committed.current = text;
    latest.current.onCommit(text);
  };

  useEffect(() => {
    let disposed = false;
    const unregister = latest.current.flushWithProfile === false ? () => {} : registerFlush(commit);
    loadEditor().then((setup) => {
      if (disposed || !host.current) return;
      const { language, indent, label, readOnly, lintDelayMs, onSave, minLines, maxLines } = latest.current;
      handle.current = setup.createEditor(host.current, {
        text: committed.current, language, indent, label, readOnly, lintDelayMs,
        minHeight: minLines ? lines(minLines) : undefined, maxHeight: maxLines ? lines(maxLines) : undefined,
        messages: {
          missing: t("codeEditor.syntax.missing"), unexpected: (text) => t("codeEditor.syntax.unexpected", { text }),
          parserSource: t("codeEditor.syntax.parser"), coreSource: t("codeEditor.syntax.core"),
        },
        onChange: (text) => {
          if (text === committed.current) { pending.current = false; if (timer.current) clearTimeout(timer.current); return; }
          if (!pending.current) latest.current.onPending?.();
          pending.current = true;
          if (timer.current) clearTimeout(timer.current);
          timer.current = setTimeout(commit, latest.current.commitDelayMs);
        },
        onBlur: commit,
        onSave: onSave ? () => { commit(); latest.current.onSave?.(); } : undefined,
      });
      if (latest.current.problems) handle.current.setExternal(latest.current.problems);
      setState("ready");
      latest.current.onReady?.(handle.current);
    }, () => { if (!disposed) setState("failed"); });
    return () => {
      disposed = true;
      commit();
      unregister();
      handle.current?.destroy();
      handle.current = null;
      latest.current.onReady?.(null);
    };
  }, []);

  // An outside change (reset, a reload from the core) replaces what is shown.
  useEffect(() => {
    if (props.value === committed.current) return;
    committed.current = props.value;
    pending.current = false;
    if (timer.current) { clearTimeout(timer.current); timer.current = null; }
    handle.current?.setText(props.value);
  }, [props.value]);

  useEffect(() => { if (props.problems && handle.current) handle.current.setExternal(props.problems); }, [props.problems]);

  if (state === "failed") return <>{props.fallback()}</>;
  return <div className={`code-editor${props.className ? ` ${props.className}` : ""}`} data-language={props.language} data-state={state}>
    {state === "loading" && <pre className="code-editor-loading" aria-busy="true" aria-label={props.label}
      style={{ minHeight: props.minLines ? lines(props.minLines) : undefined, maxHeight: props.maxLines ? lines(props.maxLines) : undefined }}>{props.value}</pre>}
    <div ref={host} className="code-editor-host" />
  </div>;
}

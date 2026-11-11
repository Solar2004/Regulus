// The code editor itself: CodeMirror 6, loaded with `import()` only when a code field or a
// source file is opened, so the first screen never pays for it. Colors come from the
// design tokens; nothing here runs, imports or evaluates the code being edited.
import { EditorState, StateEffect, StateField, type Extension } from "@codemirror/state";
import { EditorView, drawSelection, highlightActiveLine, highlightActiveLineGutter, keymap, lineNumbers } from "@codemirror/view";
import { defaultKeymap, history, historyKeymap, indentWithTab } from "@codemirror/commands";
import { HighlightStyle, bracketMatching, ensureSyntaxTree, indentOnInput, indentUnit, syntaxHighlighting, syntaxTree } from "@codemirror/language";
import { python } from "@codemirror/lang-python";
import { forceLinting, linter, lintGutter, type Diagnostic } from "@codemirror/lint";
import { tags } from "@lezer/highlight";
import { offsetsOf, parserFor, syntaxProblems, type PlacedProblem } from "./syntax";

/** The CodeMirror support of each language `parserFor` knows. */
const LANGUAGES: Record<NonNullable<ReturnType<typeof parserFor>>, () => Extension> = { python };

/** How long the parser may take per check before the lint uses what it has (ms). */
const PARSE_BUDGET_MS = 200;

export interface EditorMessages {
  missing: string;
  unexpected: (text: string) => string;
  /** Names where a problem came from: the editor's parser or the core's check. */
  parserSource: string;
  coreSource: string;
}

export interface EditorOptions {
  text: string;
  language?: string;
  /** What Tab inserts and the unit of indentation: `"\t"` or spaces. */
  indent: string;
  label: string;
  readOnly?: boolean;
  /** Milliseconds after the last change before the parser's problems are shown. */
  lintDelayMs: number;
  /** CSS max height of the scrolling area, or none to fill the parent. */
  maxHeight?: string;
  minHeight?: string;
  messages: EditorMessages;
  /** Called on every change of the text (the caller decides when to commit it). */
  onChange: (text: string) => void;
  onBlur?: () => void;
  /** Bound to Mod-s inside the editor when given; the key then stops here. */
  onSave?: () => void;
}

export interface EditorHandle {
  text(): string;
  /** Replaces the whole text with an outside change; undo can take it back. */
  setText(text: string): void;
  /** Problems found by the core's check, by line and column; mapped through later edits. */
  setExternal(problems: PlacedProblem[]): void;
  /** Puts the caret at a 1-based line and column and scrolls to it. */
  goTo(line: number, column?: number): void;
  focus(): void;
  destroy(): void;
}

const setExternalEffect = StateEffect.define<Diagnostic[]>();

/** The core's problems, kept in step with edits so they stay on the code they were about. */
const externalField = StateField.define<Diagnostic[]>({
  create: () => [],
  update(value, tr) {
    for (const effect of tr.effects) if (effect.is(setExternalEffect)) return effect.value;
    if (!tr.docChanged || !value.length) return value;
    return value.map((d) => {
      const from = tr.changes.mapPos(d.from, 1);
      return { ...d, from, to: Math.max(from, tr.changes.mapPos(d.to, -1)) };
    });
  },
});

const highlight = HighlightStyle.define([
  { tag: [tags.keyword, tags.controlKeyword, tags.definitionKeyword, tags.moduleKeyword, tags.operatorKeyword], color: "var(--rg-color-chart-2)" },
  { tag: [tags.string, tags.special(tags.string)], color: "var(--rg-color-success)" },
  { tag: [tags.number, tags.bool, tags.null], color: "var(--rg-color-warning)" },
  { tag: [tags.comment, tags.lineComment, tags.blockComment], color: "var(--rg-color-text-muted)", fontStyle: "italic" },
  { tag: [tags.function(tags.variableName), tags.function(tags.definition(tags.variableName))], color: "var(--rg-color-chart-5)" },
  { tag: [tags.className, tags.definition(tags.className)], color: "var(--rg-color-chart-4)" },
  { tag: [tags.self, tags.atom], color: "var(--rg-color-chart-1)" },
  { tag: [tags.meta, tags.annotation], color: "var(--rg-color-info)" },
  { tag: [tags.propertyName, tags.operator, tags.punctuation, tags.bracket], color: "var(--rg-color-text-secondary)" },
  { tag: tags.invalid, color: "var(--rg-color-danger)" },
]);

function theme(options: Pick<EditorOptions, "maxHeight" | "minHeight">): Extension {
  return EditorView.theme({
    "&": { color: "var(--rg-color-text)", backgroundColor: "var(--rg-color-surface)", fontSize: ".75rem", border: "1px solid var(--rg-color-control-border)", borderRadius: "var(--rg-radius-control)" },
    "&.cm-focused": { outline: "2px solid var(--rg-color-focus)", outlineOffset: "1px" },
    ".cm-scroller": { fontFamily: "var(--rg-font-mono)", lineHeight: "1.45", ...(options.maxHeight ? { maxHeight: options.maxHeight } : {}), ...(options.minHeight ? { minHeight: options.minHeight } : {}) },
    ".cm-content": { caretColor: "var(--rg-color-focus)", padding: "var(--rg-space-2) 0" },
    ".cm-cursor, .cm-dropCursor": { borderLeftColor: "var(--rg-color-focus)" },
    "&.cm-focused .cm-selectionBackground, .cm-selectionBackground, ::selection": { backgroundColor: "var(--rg-color-selected)" },
    ".cm-activeLine": { backgroundColor: "color-mix(in srgb, var(--rg-color-hover) 45%, transparent)" },
    ".cm-gutters": { backgroundColor: "var(--rg-color-canvas)", color: "var(--rg-color-text-muted)", borderRight: "1px solid var(--rg-color-border)" },
    ".cm-activeLineGutter": { backgroundColor: "var(--rg-color-hover)", color: "var(--rg-color-text)" },
    ".cm-matchingBracket": { backgroundColor: "var(--rg-color-selected)", outline: "1px solid var(--rg-color-steel)" },
    ".cm-lintRange-error": { backgroundImage: "none", textDecoration: "underline wavy var(--rg-color-danger)", textUnderlineOffset: "3px" },
    ".cm-lintRange-warning": { backgroundImage: "none", textDecoration: "underline wavy var(--rg-color-warning)", textUnderlineOffset: "3px" },
    ".cm-tooltip": { backgroundColor: "var(--rg-color-raised)", color: "var(--rg-color-text)", border: "1px solid var(--rg-color-border)", borderRadius: "var(--rg-radius-small)" },
    ".cm-diagnostic-error": { borderLeftColor: "var(--rg-color-danger)" },
    ".cm-diagnostic-warning": { borderLeftColor: "var(--rg-color-warning)" },
    ".cm-diagnostic-info": { borderLeftColor: "var(--rg-color-info)" },
    ".cm-diagnosticSource": { color: "var(--rg-color-text-muted)" },
  }, { dark: true });
}

/** Opens an editor in `parent`. The caller owns the text's fate: `onChange` reports every edit. */
export function createEditor(parent: HTMLElement, options: EditorOptions): EditorHandle {
  const parsed = parserFor(options.language);
  const language = parsed ? LANGUAGES[parsed] : undefined;
  const { messages } = options;
  const lint = linter((view) => {
    const found: Diagnostic[] = [];
    if (language) {
      const tree = ensureSyntaxTree(view.state, view.state.doc.length, PARSE_BUDGET_MS) ?? syntaxTree(view.state);
      const text = view.state.doc.toString();
      for (const problem of syntaxProblems(tree, text)) {
        found.push({
          from: problem.from, to: Math.min(text.length, Math.max(problem.to, problem.from + 1)), severity: "error", source: messages.parserSource,
          message: problem.kind === "missing" ? messages.missing : messages.unexpected(problem.text),
        });
      }
    }
    return [...found, ...view.state.field(externalField)];
  }, {
    delay: options.lintDelayMs,
    // New problems from the core are a reason to lint again even when the text did not change
    // (`forceLinting` alone only hurries a lint that is already due).
    needsRefresh: (update) => update.transactions.some((tr) => tr.effects.some((effect) => effect.is(setExternalEffect))),
  });

  const extensions: Extension[] = [
    lineNumbers(), highlightActiveLineGutter(), history(), drawSelection(), indentOnInput(), bracketMatching(), highlightActiveLine(),
    indentUnit.of(options.indent), EditorState.tabSize.of(options.indent === "\t" ? 4 : options.indent.length),
    keymap.of([
      ...(options.onSave ? [{ key: "Mod-s", run: () => { options.onSave!(); return true; }, preventDefault: true, stopPropagation: true }] : []),
      ...defaultKeymap, ...historyKeymap, indentWithTab,
    ]),
    syntaxHighlighting(highlight), theme(options), externalField, lint, lintGutter(),
    EditorView.contentAttributes.of({ "aria-label": options.label, spellcheck: "false", autocapitalize: "off", autocorrect: "off" }),
    EditorView.updateListener.of((update) => { if (update.docChanged) options.onChange(update.state.doc.toString()); }),
    EditorView.domEventHandlers({ blur: () => { options.onBlur?.(); return false; } }),
    ...(language ? [language()] : []),
    ...(options.readOnly ? [EditorState.readOnly.of(true)] : []),
  ];
  const view = new EditorView({ parent, state: EditorState.create({ doc: options.text, extensions }) });

  return {
    text: () => view.state.doc.toString(),
    setText: (text) => {
      if (text === view.state.doc.toString()) return;
      view.dispatch({ changes: { from: 0, to: view.state.doc.length, insert: text } });
    },
    setExternal: (problems) => {
      const doc = view.state.doc.toString();
      const diagnostics: Diagnostic[] = problems.map((problem) => ({ ...offsetsOf(doc, problem), severity: problem.severity ?? "error", message: problem.message, source: messages.coreSource }));
      view.dispatch({ effects: setExternalEffect.of(diagnostics) });
      forceLinting(view);
    },
    goTo: (line, column = 1) => {
      const { from } = offsetsOf(view.state.doc.toString(), { line, column });
      view.dispatch({ selection: { anchor: from }, scrollIntoView: true });
      view.focus();
    },
    focus: () => view.focus(),
    destroy: () => view.destroy(),
  };
}

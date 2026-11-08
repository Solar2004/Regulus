import en from "@/locales/en.json";

// Minimal lookup until react-i18next lands in phase B.
// Strings live in locales/*.json from day one (docs/04-client.md §10);
// this helper only reads them, it owns no text.
export type LocaleKey = keyof typeof en;

/** The string for `key`, with every `{name}` replaced by `vars.name` when given. */
export function t(key: LocaleKey, vars?: Record<string, string | number>): string {
  const text: string = en[key];
  return vars ? text.replace(/\{(\w+)\}/g, (match, name: string) => (name in vars ? String(vars[name]) : match)) : text;
}

import en from "@/locales/en.json";

// Minimal lookup until react-i18next lands in phase B.
// Strings live in locales/*.json from day one (docs/04-client.md §10);
// this helper only reads them, it owns no text.
export type LocaleKey = keyof typeof en;

export function t(key: LocaleKey): string {
  return en[key];
}

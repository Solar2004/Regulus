// The config form of a node as data: which control each field gets, what it shows and
// how typed text becomes the value the core stores. The manifest decides; a config key
// it does not know is still shown, as raw text.
import type { ConfigValue, ManifestField } from "../model";

export type Control = "text" | "number" | "select" | "map" | "workflow" | "raw";

export interface FieldRow {
  id: string;
  /** The manifest type, or `null` for a key the manifest does not declare. */
  type: string | null;
  control: Control;
  /** What the node has; `undefined` when the config does not set it. */
  value: ConfigValue | undefined;
  /** The manifest default; `undefined` when there is none. */
  defaultValue: ConfigValue | undefined;
  options: ConfigValue[];
  /** No value of its own, or the same as the default. */
  isDefault: boolean;
  secret: boolean;
}

export const SECRET_PREFIX = "secret:";
export const isSecret = (value: unknown): value is string => typeof value === "string" && value.startsWith(SECRET_PREFIX);

const BOOLEAN_TYPES = new Set(["boolean", "bool"]);
const same = (a: unknown, b: unknown) => JSON.stringify(a) === JSON.stringify(b);
const defaultOf = (field: ManifestField) => field.default === null ? undefined : field.default;

/** The control a manifest field gets; a type this app does not know falls back to raw text. */
export function controlFor(field: ManifestField): Control {
  if (field.type === "workflow") return "workflow";
  if (field.type === "select" || BOOLEAN_TYPES.has(field.type) || typeof field.default === "boolean" || (field.options?.length ?? 0) > 0) return "select";
  if (field.type === "number") return "number";
  if (field.type === "text") return "text";
  if (field.type === "map") return "map";
  return "raw";
}

function optionsFor(field: ManifestField): ConfigValue[] {
  if (field.options?.length) return field.options;
  if (BOOLEAN_TYPES.has(field.type) || typeof field.default === "boolean") return [true, false];
  return [];
}

/** One row per manifest field, in manifest order, then one per config key it does not declare. */
export function fieldRows(fields: ManifestField[] | undefined, config: Record<string, ConfigValue>): FieldRow[] {
  const known = fields ?? [];
  const rows: FieldRow[] = known.map((field) => {
    const value = config[field.id];
    const defaultValue = defaultOf(field);
    return {
      id: field.id, type: field.type, control: controlFor(field), value, defaultValue, options: optionsFor(field),
      isDefault: value === undefined || same(value, defaultValue), secret: isSecret(value ?? defaultValue),
    };
  });
  for (const [id, value] of Object.entries(config)) {
    if (known.some((field) => field.id === id)) continue;
    rows.push({ id, type: null, control: "raw", value, defaultValue: undefined, options: [], isDefault: false, secret: isSecret(value) });
  }
  return rows;
}

/** A config holding every default the manifest gives (a node just added). */
export function defaultsOf(fields: ManifestField[] | undefined): Record<string, ConfigValue> {
  const config: Record<string, ConfigValue> = {};
  for (const field of fields ?? []) {
    const value = defaultOf(field);
    if (value !== undefined) config[field.id] = structuredClone(value);
  }
  return config;
}

/** What "reset" writes: the default, or nothing (`undefined` removes the key). */
export const resetValue = (row: FieldRow): ConfigValue | undefined => row.defaultValue === undefined ? undefined : structuredClone(row.defaultValue);

/** How a value reads in a text control: strings as they are, the rest as JSON. */
export const displayValue = (value: ConfigValue | undefined): string => value === undefined ? "" : typeof value === "string" ? value : JSON.stringify(value);

export type Coerced = { ok: true; value: ConfigValue } | { ok: false; error: "number" | "json" | "option" };

/** Turns what was typed or picked into the value to store, keeping the type the field expects. */
export function coerce(row: Pick<FieldRow, "control" | "value" | "defaultValue" | "options">, input: string): Coerced {
  switch (row.control) {
    case "number": {
      const value = Number(input);
      return input.trim() !== "" && Number.isFinite(value) ? { ok: true, value } : { ok: false, error: "number" };
    }
    case "select": {
      const option = row.options.find((item) => displayValue(item) === input);
      return option === undefined ? { ok: false, error: "option" } : { ok: true, value: option };
    }
    case "raw": {
      const current = row.value ?? row.defaultValue;
      if (current === undefined || typeof current === "string") return { ok: true, value: input };
      try {
        const value = JSON.parse(input) as ConfigValue | null;
        return value === null ? { ok: false, error: "json" } : { ok: true, value };
      } catch { return { ok: false, error: "json" }; }
    }
    default: return { ok: true, value: input };
  }
}

/** The rows of a `map` field: each key with its value as text. */
export function mapEntries(value: ConfigValue | undefined): [string, string][] {
  if (!value || typeof value !== "object" || Array.isArray(value)) return [];
  return Object.entries(value).map(([key, item]) => [key, displayValue(item)]);
}

/** A map from its rows; rows without a key are left out, the last of two equal keys wins. */
export function mapFrom(entries: [string, string][]): Record<string, ConfigValue> {
  const map: Record<string, ConfigValue> = {};
  for (const [key, value] of entries) if (key.trim()) map[key.trim()] = value;
  return map;
}

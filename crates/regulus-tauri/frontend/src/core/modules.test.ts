import { describe, expect, test } from "vitest";
import { moduleCountKey } from "./modules";
import type { ModuleDefinition } from "@/composition/model";

const module = (id: string): ModuleDefinition => ({ id, name: id, node_types: [], workflow_id: null });

describe("module count line", () => {
  test("says unknown before the core answers, then counts", () => {
    expect(moduleCountKey(null)).toBe("settings.modulesUnknown");
    expect(moduleCountKey([])).toBe("settings.modulesEmpty");
    expect(moduleCountKey([module("a")])).toBe("settings.modulesLoadedOne");
    expect(moduleCountKey([module("a"), module("b")])).toBe("settings.modulesLoaded");
  });
});

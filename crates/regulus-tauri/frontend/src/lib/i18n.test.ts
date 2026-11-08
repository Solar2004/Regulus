import { describe, expect, test } from "vitest";
import { t } from "./i18n";

describe("t", () => {
  test("fills {name} holes and leaves unknown ones as written", () => {
    expect(t("settings.modulesLoaded", { count: 22 })).toBe("22 modules loaded");
    expect(t("settings.modulesLoaded")).toBe("{count} modules loaded");
    expect(t("settings.modulesLoaded", { other: 1 })).toBe("{count} modules loaded");
  });
});

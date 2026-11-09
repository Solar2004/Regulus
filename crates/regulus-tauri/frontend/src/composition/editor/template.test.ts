import { describe, expect, test } from "vitest";
import { emptyTemplateForm, templateSaveErrorOf, templateSaveParams } from "./template";

describe("save as template", () => {
  test("the params carry only what was filled in", () => {
    expect(templateSaveParams("p", "main", emptyTemplateForm(" chat "))).toEqual({ ok: true, params: { name: "chat", profile_id: "p" } });
    const full = templateSaveParams("p", "inner", {
      name: "chat", description: " A chat ", onlyWorkflow: true, path: " /tmp/t.toml ", force: true,
      vars: [{ name: "model", description: "Which model", default: "" }, { name: "greeting", description: "", default: "hi" }],
    });
    expect(full).toEqual({ ok: true, params: {
      name: "chat", profile_id: "p", description: "A chat", workflow_id: "inner", path: "/tmp/t.toml", force: true,
      vars: { model: { description: "Which model" }, greeting: { description: "", default: "hi" } },
    } });
    // "Only this workflow" without one on screen (all workflows) sends the whole profile.
    expect(templateSaveParams("p", null, { ...emptyTemplateForm("x"), onlyWorkflow: true })).toEqual({ ok: true, params: { name: "x", profile_id: "p" } });
  });

  test("an empty name, a var name that is not a word, or one used twice stop it", () => {
    expect(templateSaveParams("p", null, emptyTemplateForm("  "))).toEqual({ ok: false, problem: "name" });
    const row = (name: string) => ({ name, description: "", default: "" });
    expect(templateSaveParams("p", null, { ...emptyTemplateForm("x"), vars: [row("ok"), row("2bad")] })).toEqual({ ok: false, problem: "varName", row: 1 });
    expect(templateSaveParams("p", null, { ...emptyTemplateForm("x"), vars: [row("has space")] })).toEqual({ ok: false, problem: "varName", row: 0 });
    expect(templateSaveParams("p", null, { ...emptyTemplateForm("x"), vars: [row("a"), row(" a ")] })).toEqual({ ok: false, problem: "varDuplicate", row: 1 });
  });

  test("errors are told apart by code", () => {
    expect(templateSaveErrorOf({ code: -32007 })).toBe("exists");
    expect(templateSaveErrorOf({ code: -32005 })).toBe("invalid");
    expect(templateSaveErrorOf({ code: -32601 })).toBe("unsupported");
    expect(templateSaveErrorOf({ code: -1 })).toBe("other");
    expect(templateSaveErrorOf(new Error("x"))).toBe("other");
  });
});

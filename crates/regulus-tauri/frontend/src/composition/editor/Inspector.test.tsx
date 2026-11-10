import { renderToStaticMarkup } from "react-dom/server";
import { describe, expect, test } from "vitest";
import { t } from "@/lib/i18n";
import type { Manifest } from "../model";
import { node, port, sample } from "./fixtures.test-util";
import { NodeInspector } from "./Inspector";

const manifest: Manifest = {
  module: { id: "me/py@0.1.0", name: "Py" },
  node: [{
    type: "upper", name: "Upper", port: [port("in", "input"), port("out", "output")],
    field: [
      { id: "source", type: "code", default: "def run(ctx, port, value):\n    return value", language: "python", label: "Source", description: "Runs once per event" },
      { id: "notes", type: "text", multiline: true, default: "a\nb" },
      { id: "path", type: "text", default: "out.md" },
    ],
  }],
};

function render(config: Record<string, string> = {}) {
  const profile = sample();
  const py = node("py", [port("in", "input"), port("out", "output")], { type: "upper", module_id: "me/py@0.1.0", config });
  profile.workflows[0].nodes.push(py);
  const noop = () => {};
  return renderToStaticMarkup(<NodeInspector profile={profile} workflowId="main" node={py} manifests={[manifest]} modules={[]} diagnostics={[]}
    edit={noop} onOpenWorkflow={noop} onCreateBox={noop} onDelete={noop} onClose={noop} />);
}

const escape = (text: string) => text.replace(/&/g, "&amp;").replace(/</g, "&lt;").replace(/>/g, "&gt;");

describe("inspector text areas", () => {
  test("a code field is a monospaced text area with its language, label, help and the plain-text notice", () => {
    const html = render();
    expect(html).toMatch(/<textarea[^>]*class="field-textarea code-input"[^>]*aria-label="Source"[^>]*rows="4"[^>]*wrap="off"[^>]*spellCheck="false"[^>]*data-language="python"/);
    expect(html).toContain("def run(ctx, port, value):\n    return value</textarea>");
    expect(html).toContain('<span class="config-label">Source</span><span class="config-id">source</span>');
    expect(html).toContain("code · python");
    expect(html).toContain("Runs once per event");
    expect(html).toContain(escape(t("editor.field.codePlain")));
    // The default is the text shown; it is not repeated under the box.
    expect(html).not.toContain(`${t("editor.field.default")} <code>def run`);
  });

  test("a multiline text field is a proportional text area without the code notice", () => {
    const html = render();
    expect(html).toMatch(/<textarea[^>]*class="field-textarea"[^>]*aria-label="notes"[^>]*rows="3"[^>]*wrap="soft"/);
    expect(html.match(new RegExp(escape(t("editor.field.codePlain")).replace(/[.*+?^${}()|[\]\\]/g, "\\$&"), "g"))).toHaveLength(1);
  });

  test("a plain text field stays a one-line input and the node's own value wins over the default", () => {
    const html = render({ source: "x = 1" });
    expect(html).toMatch(/<input aria-label="path" value="out.md"\/>/);
    expect(html).toContain(">x = 1</textarea>");
    // Once the value differs, the default shows again so the user sees what reset gives back.
    expect(html).toContain(`${t("editor.field.default")} <code>def run`);
  });
});

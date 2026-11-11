import { compile } from "tailwindcss";
import tailwindCss from "tailwindcss/index.css?raw";
import { describe, expect, test } from "vitest";
import appCss from "../../index.css?raw";
import recipesCss from "../../../../../../assets/design/centaury-recipes.css?raw";
import themeCss from "../../../../../../assets/design/shadcn-theme.css?raw";
import tokensCss from "../../../../../../assets/design/tokens.css?raw";
import { dialogClasses } from "./dialog";

/** What `src/index.css` imports, by the id it imports it with. */
const imported: Record<string, string> = {
  tailwindcss: tailwindCss,
  "../../../../assets/design/tokens.css": tokensCss,
  "../../../../assets/design/shadcn-theme.css": themeCss,
  "../../../../assets/design/centaury-recipes.css": recipesCss,
};

/** The app's own stylesheet (Tailwind + the Regulus theme), compiled the way Vite does. */
function compileApp() {
  return compile(appCss, {
    loadStylesheet: async (id, base) => {
      if (!(id in imported)) throw new Error(`index.css imports ${id}: add it to this test`);
      return { path: id, base, content: imported[id] };
    },
  });
}

const tokens = [...new Set(Object.values(dialogClasses).flatMap((list) => list.split(/\s+/)))];

describe("dialog styles", () => {
  test("every class exists in the Regulus theme: an unknown colour leaves the panel transparent", async () => {
    const compiler = await compileApp();
    let css = compiler.build([]);
    const missing: string[] = [];
    for (const token of tokens) {
      const next = compiler.build([token]);
      if (next === css) missing.push(token);
      css = next;
    }
    expect(missing).toEqual([]);
  });

  test("colours come from the theme tokens, never from Tailwind's built-in palette", () => {
    const builtIn = /(?:^|:)(?:bg|text|border|ring|shadow|outline|fill|stroke)-(?:black|white|(?:slate|gray|zinc|neutral|stone|red|orange|amber|yellow|lime|green|emerald|teal|cyan|sky|blue|indigo|violet|purple|fuchsia|pink|rose)-\d+)(?:\/|$)/;
    expect(tokens.filter((token) => builtIn.test(token))).toEqual([]);
    expect(dialogClasses.backdrop).toContain("bg-overlay");
    expect(dialogClasses.popup).toContain("bg-surface");
    expect(tokens.some((token) => token.includes("blur"))).toBe(false);
  });

  test("scrollbars behind an open modal are hidden: WebKit would paint them across the panel", async () => {
    const css = (await compileApp()).build([]);
    expect(css).toMatch(/\[data-base-ui-inert\] \*\s*\{\s*scrollbar-color: transparent transparent;/);
  });

  test("a tall dialog scrolls inside the window and keeps its title and actions on screen", () => {
    expect(dialogClasses.viewport).toMatch(/\boverflow-y-auto\b/);
    expect(dialogClasses.popup).toMatch(/\bmax-h-\[max\(100%,/);
    expect(dialogClasses.popup).toMatch(/\bmy-auto\b/);
    expect(dialogClasses.body).toMatch(/\boverflow-y-auto\b/);
    expect(dialogClasses.body).toMatch(/\bmin-h-0\b/);
    expect(dialogClasses.header).toMatch(/\bsticky -top-6 z-10 -mt-6\b/);
    expect(dialogClasses.footer).toMatch(/\bsticky -bottom-6 z-10 -mb-6\b/);
    // Stuck parts cover what scrolls under them only if they are opaque.
    expect(dialogClasses.header).toContain("bg-surface");
    expect(dialogClasses.footer).toContain("bg-surface");
  });
});

import { describe, expect, test } from "vitest";
import { addWire } from "./document";
import { canConnect } from "./connect";
import { sample } from "./fixtures.test-util";

const end = (node_id: string, port_id: string) => ({ node_id, port_id });

describe("canConnect", () => {
  test("same capability and kind connect, with the kind of the ports", () => {
    expect(canConnect(sample(), "main", end("a", "greeting"), end("c", "greeting"))).toEqual({ ok: true, kind: "uses" });
    expect(canConnect(sample(), "main", end("a", "tick"), end("a", "echo"))).toMatchObject({ ok: false, reason: "sameNode" });
  });

  test("a capability mismatch is refused", () => {
    const result = canConnect(sample(), "main", end("b", "written"), end("a", "echo"));
    expect(result).toMatchObject({ ok: false, reason: "capability" });
    if (!result.ok) expect([result.from?.capability, result.to?.capability]).toEqual(["path@1", "text@1"]);
  });

  test("a kind mismatch is refused", () => {
    expect(canConnect(sample(), "main", end("a", "tick"), end("c", "greeting"))).toMatchObject({ ok: false, reason: "kind" });
  });

  test("direction and missing ports are refused", () => {
    expect(canConnect(sample(), "main", end("b", "in"), end("a", "echo"))).toMatchObject({ ok: false, reason: "direction" });
    expect(canConnect(sample(), "main", end("a", "nope"), end("b", "in"))).toMatchObject({ ok: false, reason: "missingPort" });
    expect(canConnect(sample(), "nope", end("a", "tick"), end("b", "in"))).toMatchObject({ ok: false, reason: "missingPort" });
  });

  test("an input of cardinality one takes a single wire; many takes more", () => {
    // b.in already has w1 from a.tick.
    expect(canConnect(sample(), "main", end("box", "r"), end("b", "in"))).toMatchObject({ ok: false, reason: "taken" });
    let profile = addWire(sample(), "main", "then", end("self", "ask"), end("a", "echo"));
    expect(canConnect(profile, "main", end("box", "r"), end("a", "echo"))).toEqual({ ok: true, kind: "then" });
    profile = addWire(profile, "main", "then", end("box", "r"), end("a", "echo"));
    expect(canConnect(profile, "main", end("box", "r"), end("a", "echo"))).toMatchObject({ ok: false, reason: "duplicate" });
  });

  test("self ports face inward: a workflow input feeds nodes, an output takes one wire", () => {
    const profile = sample();
    expect(canConnect(profile, "main", end("self", "ask"), end("b", "in"))).toMatchObject({ ok: false, reason: "taken" });
    expect(canConnect(profile, "main", end("self", "ask"), end("a", "echo"))).toEqual({ ok: true, kind: "then" });
    expect(canConnect(profile, "main", end("a", "tick"), end("self", "reply"))).toEqual({ ok: true, kind: "then" });
    expect(canConnect(profile, "main", end("self", "reply"), end("a", "echo"))).toMatchObject({ ok: false, reason: "direction" });
    expect(canConnect(profile, "inner", end("h", "reply"), end("self", "r"))).toMatchObject({ ok: false, reason: "duplicate" });
    // A workflow input straight to its output is a pass-through, not "the same node".
    expect(canConnect(profile, "main", end("self", "ask"), end("self", "reply"))).toEqual({ ok: true, kind: "then" });
  });

  test("a box offers its workflow's ports", () => {
    expect(canConnect(sample(), "main", end("box", "r"), end("a", "echo"))).toEqual({ ok: true, kind: "then" });
    expect(canConnect(sample(), "main", end("a", "tick"), end("box", "q"))).toEqual({ ok: true, kind: "then" });
  });
});

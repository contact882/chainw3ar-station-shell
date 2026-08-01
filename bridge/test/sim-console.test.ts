// Pins the fix for the 2026-08-01 incident where QUIT SHELL sat below the fold of the
// 720px-tall sim window (fails if quit ever leaves the fixed footer).

import { readFileSync } from "node:fs";
import { fileURLToPath } from "node:url";
import { describe, expect, it } from "vitest";

const html = readFileSync(
  fileURLToPath(new URL("../sim-console/sim.html", import.meta.url)),
  "utf8",
);

describe("sim console layout", () => {
  it("keeps the quit button inside the fixed #shell-footer", () => {
    const open = html.indexOf('<div id="shell-footer"');
    expect(open).toBeGreaterThan(-1);
    const close = html.indexOf("</div>", open);
    expect(close).toBeGreaterThan(open);
    const footer = html.slice(open, close);
    expect(footer).toContain('id="quit"');
  });

  it("styles #shell-footer as position: fixed", () => {
    const rule = html.match(/#shell-footer\s*\{[^}]*\}/);
    expect(rule).not.toBeNull();
    expect(rule![0]).toContain("position: fixed");
  });

  it("pads the body bottom so flow content clears the footer", () => {
    expect(html).toMatch(/body\s*\{[^}]*padding-bottom/);
  });
});

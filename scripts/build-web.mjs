// Builds the three web artifacts and assembles app-dist/ (the frontendDist):
//   1. bridge/src/init.ts      → src-tauri/gen/init_script.js  (IIFE, include_str!'d)
//   2. bridge/harness/*        → app-dist/harness/
//   3. bridge/sim-console/*    → app-dist/sim/
//   4. ui-dist/*               → app-dist/   (the frozen UI, if fetched)
import { build } from "esbuild";
import { cpSync, existsSync, mkdirSync, readFileSync, rmSync, writeFileSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";

const root = join(dirname(fileURLToPath(import.meta.url)), "..");
const appDist = join(root, "app-dist");
const uiDist = join(root, "ui-dist");

// 1. Init script: single IIFE. NOT minified — the "__BAKED_BOOT_JSON__"
// placeholder must survive verbatim for Rust's window-creation bake.
await build({
  entryPoints: [join(root, "bridge/src/init.ts")],
  bundle: true,
  format: "iife",
  target: "es2020",
  outfile: join(root, "src-tauri/gen/init_script.js"),
  legalComments: "none",
});
const initJs = readFileSync(join(root, "src-tauri/gen/init_script.js"), "utf8");
if (!initJs.includes('"__BAKED_BOOT_JSON__"')) {
  console.error("FATAL: __BAKED_BOOT_JSON__ placeholder lost in bundling");
  process.exit(1);
}

// 2+3. Harness and sim pages.
rmSync(appDist, { recursive: true, force: true });
mkdirSync(join(appDist, "harness"), { recursive: true });
mkdirSync(join(appDist, "sim"), { recursive: true });
await build({
  entryPoints: [join(root, "bridge/harness/conformance-main.ts")],
  bundle: true,
  format: "esm",
  target: "es2020",
  outfile: join(appDist, "harness/conformance-main.js"),
});
await build({
  entryPoints: [join(root, "bridge/sim-console/sim-main.ts")],
  bundle: true,
  format: "esm",
  target: "es2020",
  outfile: join(appDist, "sim/sim-main.js"),
});
cpSync(join(root, "bridge/harness/conformance.html"), join(appDist, "harness/conformance.html"));
cpSync(join(root, "bridge/sim-console/sim.html"), join(appDist, "sim/sim.html"));

// 4. The frozen UI.
if (existsSync(join(uiDist, "index.html"))) {
  cpSync(uiDist, appDist, { recursive: true });
} else {
  writeFileSync(
    join(appDist, "index.html"),
    "<!doctype html><meta charset=utf-8><title>Station</title>" +
      "<p>ui-dist missing — run <code>npm run fetch-ui</code> then rebuild.</p>",
  );
  console.warn("WARN: ui-dist missing — app-dist has a placeholder index.html");
}
console.log("app-dist assembled; init script " + initJs.length + " bytes");

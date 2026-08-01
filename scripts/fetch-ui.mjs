// Produces ui-dist/ — the frozen UI's production build — reproducibly.
//
// The UI repo pins node ">=20.11 <21" with engine-strict; that pin is
// load-bearing (the toolchain was validated on exactly that runtime), so this
// script uses a portable Node 20.11.1 rather than overriding the pin with
// whatever Node the shell machine happens to run. `--allow-system-node` exists
// as a loud emergency fallback only.
import { createHash } from "node:crypto";
import { execFileSync, spawnSync } from "node:child_process";
import {
  cpSync, existsSync, mkdirSync, readFileSync, rmSync, writeFileSync,
} from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";

const root = join(dirname(fileURLToPath(import.meta.url)), "..");
const pin = JSON.parse(readFileSync(join(root, "bridge", "vendor", "PIN.json"), "utf8"));
const uiSrc = join(root, "vendor", "ui-src");
const uiDist = join(root, "ui-dist");
const toolsDir = join(root, "tools");
const stampPath = join(uiDist, ".build-info.json");
const allowSystemNode = process.argv.includes("--allow-system-node");
const clean = process.argv.includes("--clean");

const NODE_VERSION = "20.11.1";
const NODE_DIR = join(toolsDir, `node-v${NODE_VERSION}-win-x64`);
const NODE_ZIP_URL = `https://nodejs.org/dist/v${NODE_VERSION}/node-v${NODE_VERSION}-win-x64.zip`;
// From https://nodejs.org/dist/v20.11.1/SHASUMS256.txt
const NODE_ZIP_SHA256 = "bc032628d77d206ffa7f133518a6225a9c5d6d9210ead30d67e294ff37044bda";

const git = (...args) => execFileSync("git", ["-C", uiSrc, ...args], { encoding: "utf8" }).trim();

// 0. Early exit when the stamp already matches the pin.
if (!clean && existsSync(stampPath)) {
  const stamp = JSON.parse(readFileSync(stampPath, "utf8"));
  if (stamp.tag === pin.tag && stamp.commit === pin.commit && existsSync(join(uiDist, "index.html"))) {
    console.log(`ui-dist up to date (${stamp.tag} @ ${stamp.commit.slice(0, 8)}, node ${stamp.node}) — nothing to do`);
    process.exit(0);
  }
}

// 1. Clone or reuse the UI source at the pinned tag.
if (!existsSync(join(uiSrc, ".git"))) {
  mkdirSync(dirname(uiSrc), { recursive: true });
  const from = process.env.UI_CLONE_FROM ?? pin.repo;
  console.log(`cloning ${from} -> vendor/ui-src`);
  execFileSync("git", ["clone", "--quiet", from, uiSrc], { stdio: "inherit" });
  if (from !== pin.repo) execFileSync("git", ["-C", uiSrc, "remote", "set-url", "origin", pin.repo]);
}
const tagCommit = git("rev-parse", `${pin.tag}^{commit}`);
if (tagCommit !== pin.commit) {
  console.error(`FATAL: tag ${pin.tag} resolves to ${tagCommit}, pinned ${pin.commit}. Tag moved?`);
  process.exit(1);
}
git("checkout", "--detach", "--quiet", pin.tag);
if (clean) git("clean", "-fdxq");

// 2. Portable Node 20.11.1 (download once, verify SHA-256, cache in tools/).
let nodeBinDir = NODE_DIR;
if (allowSystemNode) {
  console.warn("WARNING: --allow-system-node — building the frozen UI on an UNVALIDATED Node runtime.");
  nodeBinDir = null;
} else if (!existsSync(join(NODE_DIR, "npm.cmd"))) {
  mkdirSync(toolsDir, { recursive: true });
  const zipPath = join(toolsDir, `node-v${NODE_VERSION}-win-x64.zip`);
  console.log(`downloading ${NODE_ZIP_URL}`);
  const dl = spawnSync("curl", ["-fsSL", "-o", zipPath, NODE_ZIP_URL], { stdio: "inherit" });
  if (dl.status !== 0) { console.error("FATAL: node zip download failed"); process.exit(1); }
  const actual = createHash("sha256").update(readFileSync(zipPath)).digest("hex");
  if (actual !== NODE_ZIP_SHA256) {
    rmSync(zipPath);
    console.error(`FATAL: node zip SHA-256 mismatch (${actual})`);
    process.exit(1);
  }
  const unzip = spawnSync(
    "powershell.exe",
    ["-NoProfile", "-Command",
     `Expand-Archive -LiteralPath '${zipPath}' -DestinationPath '${toolsDir}' -Force`],
    { stdio: "inherit" },
  );
  if (unzip.status !== 0 || !existsSync(join(NODE_DIR, "npm.cmd"))) {
    console.error("FATAL: node zip extraction failed");
    process.exit(1);
  }
  rmSync(zipPath);
}

// 3. npm ci && npm run build with the pinned runtime (production mode ONLY —
//    build:e2e would ship dev hooks like __MOCK_CONTROLS__).
// Invoke the pinned node.exe against npm-cli.js directly — Node >=21 refuses
// to spawn .cmd shims without a shell, and a shell would re-resolve PATH.
const nodeExe = nodeBinDir ? join(nodeBinDir, "node.exe") : process.execPath;
const npmCli = nodeBinDir
  ? join(nodeBinDir, "node_modules", "npm", "bin", "npm-cli.js")
  : "npm-cli.js";
const env = nodeBinDir
  ? { ...process.env, PATH: `${nodeBinDir};${process.env.PATH}` }
  : process.env;
for (const args of [["ci"], ["run", "build"]]) {
  console.log(`ui-src> npm ${args.join(" ")}`);
  const r = spawnSync(nodeExe, [npmCli, ...args], { cwd: uiSrc, env, stdio: "inherit" });
  if (r.status !== 0) {
    console.error(`FATAL: npm ${args.join(" ")} failed`, r.error ?? `(exit ${r.status})`);
    process.exit(1);
  }
}

// 4. Copy dist -> ui-dist and stamp.
rmSync(uiDist, { recursive: true, force: true });
cpSync(join(uiSrc, "dist"), uiDist, { recursive: true });
const nodeUsed = nodeBinDir ? NODE_VERSION : `system:${process.version}`;
writeFileSync(
  stampPath,
  JSON.stringify({ tag: pin.tag, commit: pin.commit, node: nodeUsed, builtAt: new Date().toISOString() }, null, 2),
);

// 5. Cross-check the vendored contract files against the exact source just built.
const normalize = (s) => s.toString("utf8").replaceAll("\r\n", "\n");
for (const file of Object.keys(pin.sha256)) {
  const fromTag = execFileSync("git", ["-C", uiSrc, "show", `${pin.tag}:src/station/${file}`], {
    encoding: "buffer", maxBuffer: 1 << 22,
  });
  const vendored = readFileSync(join(root, "bridge", "vendor", file));
  if (normalize(fromTag) !== normalize(vendored)) {
    console.error(`FATAL: vendored ${file} differs from the ${pin.tag} source just built`);
    process.exit(1);
  }
}
console.log(`ui-dist ready (${pin.tag} @ ${pin.commit.slice(0, 8)}, node ${nodeUsed})`);

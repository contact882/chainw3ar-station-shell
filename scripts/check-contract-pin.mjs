// Contract pin gate. The two vendored files are byte-copies of the UI repo's
// frozen tag; this script fails the build if they drift by a single byte
// (after CRLF->LF normalization, so a Windows checkout can't false-positive).
// Mirrored by a Rust #[test] reading the same PIN.json — no duplicated hashes.
import { createHash } from "node:crypto";
import { readFileSync, existsSync } from "node:fs";
import { execFileSync } from "node:child_process";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";

const root = join(dirname(fileURLToPath(import.meta.url)), "..");
const vendorDir = join(root, "bridge", "vendor");
const pin = JSON.parse(readFileSync(join(vendorDir, "PIN.json"), "utf8"));

const normalize = (buf) => buf.toString("utf8").replaceAll("\r\n", "\n");
const sha256 = (text) => createHash("sha256").update(text).digest("hex");

let failed = false;
const fail = (msg) => {
  failed = true;
  console.error(`PIN GATE FAIL: ${msg}`);
};

for (const [file, expected] of Object.entries(pin.sha256)) {
  const path = join(vendorDir, file);
  const actual = sha256(normalize(readFileSync(path)));
  if (actual !== expected) {
    fail(
      `${file} hash mismatch\n  expected ${expected}\n  actual   ${actual}\n` +
        `  The contract is frozen at ${pin.tag}. Never edit the vendored copies; ` +
        `a real contract change bumps CONTRACT_VERSION upstream and re-pins.`,
    );
  }
}

const contractSource = readFileSync(join(vendorDir, "contract.ts"), "utf8");
if (!/export const CONTRACT_VERSION = 1;/.test(contractSource)) {
  fail(`contract.ts does not declare CONTRACT_VERSION = ${pin.contractVersion}`);
}

// Optional deeper check when the UI clone is present: byte-compare against the
// tag blob and verify the tag still points at the pinned commit.
const uiSrc = process.env.UI_SRC ?? join(root, "vendor", "ui-src");
if (existsSync(join(uiSrc, ".git"))) {
  try {
    const tagCommit = execFileSync("git", ["-C", uiSrc, "rev-parse", `${pin.tag}^{commit}`], {
      encoding: "utf8",
    }).trim();
    if (tagCommit !== pin.commit) {
      fail(`tag ${pin.tag} moved: pinned ${pin.commit}, clone has ${tagCommit}`);
    }
    for (const file of Object.keys(pin.sha256)) {
      const blob = execFileSync(
        "git",
        ["-C", uiSrc, "show", `${pin.tag}:src/station/${file}`],
        { encoding: "buffer", maxBuffer: 1 << 22 },
      );
      if (normalize(blob) !== normalize(readFileSync(join(vendorDir, file)))) {
        fail(`${file} differs from ${pin.tag} blob in the UI clone`);
      }
    }
  } catch (err) {
    fail(`UI clone cross-check errored: ${err.message ?? err}`);
  }
}

if (failed) process.exit(1);
console.log(
  `contract pin OK (${pin.tag} @ ${pin.commit.slice(0, 8)}, ` +
    `contract.ts ${pin.sha256["contract.ts"].slice(0, 8)}…, ` +
    `conformance.ts ${pin.sha256["conformance.ts"].slice(0, 8)}…, CONTRACT_VERSION=${pin.contractVersion})`,
);

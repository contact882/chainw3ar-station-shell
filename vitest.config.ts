import { defineConfig } from "vitest/config";

export default defineConfig({
  test: {
    // Shell bridge tests only — vendor/ui-src is the frozen UI's own repo
    // with its own suite (run there, on its own toolchain, not here).
    include: ["bridge/test/**/*.test.ts"],
    environment: "node",
  },
});

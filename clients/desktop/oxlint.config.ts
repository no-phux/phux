import { defineConfig } from "oxlint";

export default defineConfig({
  plugins: ["typescript", "import", "unicorn", "oxc"],
  jsPlugins: [{ name: "anti-slop", specifier: "./tools/oxlint/anti-slop/index.ts" }],
  ignorePatterns: [
    "node_modules/**",
    "toolchain/**",
    "native/**",
    "dist/**",
    "tools/oxlint/anti-slop/**",
  ],
  rules: {
    "typescript/no-explicit-any": "error",
    "typescript/no-unsafe-assignment": "error",
    "typescript/no-unsafe-argument": "error",
    "typescript/no-unsafe-call": "error",
    "typescript/no-unsafe-member-access": "error",
    "typescript/no-unsafe-return": "error",
    "typescript/no-floating-promises": ["error", { ignoreVoid: false }],
    "typescript/no-misused-promises": "error",
    "typescript/ban-ts-comment": "error",
    "anti-slop/no-chained-type-assertions": "error",
    "anti-slop/no-module-mocking": "error",
    "anti-slop/require-safety-comment-for-type-assertion": "error",
    "no-restricted-imports": [
      "error",
      {
        paths: [
          {
            name: "bun:test",
            importNames: ["mock"],
            message: "Use real dependency seams instead of module mocks.",
          },
        ],
        patterns: [
          {
            group: ["**/cockpit/**", "**/phux-web/**", "**/phux-vt-web/**"],
            message: "Desktop tooling must not import another client's implementation.",
          },
        ],
      },
    ],
  },
});

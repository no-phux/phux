import { defineConfig } from "oxlint";

// The desktop tooling's type-aware rule set. Its anti-slop JS plugin resolves
// against the desktop package's own dependencies, so it stays there.
export default defineConfig({
  plugins: ["typescript", "import", "unicorn", "oxc"],
  ignorePatterns: ["node_modules/**"],
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
      },
    ],
  },
});

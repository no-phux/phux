import { defineConfig } from "oxlint";

// The tools' type-aware rule set.
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
  overrides: [
    {
      // The design generator walks the token sources as plain JSON; the
      // closed schema (phux.tokens.schema.json) checks their shape at run time.
      files: ["src/design/**"],
      rules: {
        "typescript/no-explicit-any": "off",
        "typescript/no-unsafe-assignment": "off",
        "typescript/no-unsafe-argument": "off",
        "typescript/no-unsafe-call": "off",
        "typescript/no-unsafe-member-access": "off",
        "typescript/no-unsafe-return": "off",
      },
    },
  ],
});

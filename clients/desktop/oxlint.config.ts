import { defineConfig } from "oxlint";

export default defineConfig({
  plugins: ["typescript", "import", "unicorn", "oxc"],
  jsPlugins: [
    { name: "solid", specifier: "eslint-plugin-solid" },
    { name: "anti-slop", specifier: "./tools/oxlint/anti-slop/index.ts" },
  ],
  ignorePatterns: [
    "node_modules/**",
    "toolchain/**",
    "native/**",
    "dist/**",
    "tools/oxlint/anti-slop/**",
  ],
  settings: { solid: { moduleSources: ["solid-js", "@gpuix/solid"] } },
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
    "solid/reactivity": "error",
    "solid/no-destructure": "error",
    "solid/components-return-once": "error",
    "solid/prefer-for": "error",
    "solid/jsx-no-duplicate-props": "error",
    "no-restricted-imports": [
      "error",
      {
        paths: [
          {
            name: "solid-js/web",
            message: "GPUIX uses the Solid universal renderer, not the DOM renderer.",
          },
          {
            name: "bun:test",
            importNames: ["mock"],
            message: "Use real dependency seams instead of module mocks.",
          },
        ],
        patterns: [
          {
            group: [
              "solid-js/web/**",
              "solid-js/h",
              "solid-js/h/**",
              "solid-js/html",
              "solid-js/html/**",
            ],
            message: "Use GPUIX's Solid universal renderer instead of a DOM entrypoint.",
          },
          {
            group: ["**/cockpit/**", "**/phux-web/**", "**/phux-vt-web/**"],
            message:
              "Desktop must consume its binding contract, not another client's implementation.",
          },
        ],
      },
    ],
  },
  overrides: [
    {
      files: ["src/**/*.{ts,tsx}", "tests/tooling/negative/**/*.{ts,tsx}"],
      rules: {
        "import/no-nodejs-modules": "error",
      },
    },
    {
      files: [
        "src/bridge/**/*.{ts,tsx}",
        "src/services/**/*.{ts,tsx}",
        "tests/tooling/positive/services/**/*.ts",
        "tests/tooling/negative/services/**/*.ts",
      ],
      rules: {
        "import/no-nodejs-modules": [
          "error",
          { allow: ["node:path", "node:module", "node:fs/promises"] },
        ],
      },
    },
  ],
});

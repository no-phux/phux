import { definePlugin } from "@oxlint/plugins";
import { noChainedTypeAssertionsRule } from "./rules/no-chained-type-assertions.ts";
import { noModuleMockingRule } from "./rules/no-module-mocking.ts";
import { requireSafetyCommentForTypeAssertionRule } from "./rules/require-safety-comment-for-type-assertion.ts";

export default definePlugin({
  meta: { name: "anti-slop" },
  rules: {
    "no-chained-type-assertions": noChainedTypeAssertionsRule,
    "no-module-mocking": noModuleMockingRule,
    "require-safety-comment-for-type-assertion": requireSafetyCommentForTypeAssertionRule,
  },
});

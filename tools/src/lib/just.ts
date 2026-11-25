// Recipes as `just --dump` reports them, so imports resolve the way `just`
// itself resolves them.
import { capture } from "./process.ts";
import { ROOT } from "./repo.ts";
import { list, table } from "./value.ts";

/** A recipe's body lines, interpolations flattened; undefined when there is no such recipe. */
export function recipeBody(name: string, cwd = ROOT): string | undefined {
  const dump = capture(["just", "--dump", "--dump-format", "json"], { cwd });
  const recipe = table(table(JSON.parse(dump))["recipes"])[name];
  if (recipe === undefined) return undefined;
  return list(table(recipe)["body"])
    .map((line) => list(line).map(String).join(""))
    .join("\n");
}

/**
 * tokens/swift/Localizable.xcstrings: a String Catalog with the status words (`status.<word>`)
 * and the count plurals (`count.<tone>`, `%lld`). JSON carries no header; the consumer marks it
 * linguist-generated.
 */
import { icuForms } from "./plurals.ts";

const unit = (value: string) => ({ stringUnit: { state: "translated", value } });

export function emitStrings(vocabulary: {
  words: Record<string, string>;
  plurals: Record<string, string>;
}): string {
  const strings: Record<string, unknown> = {};
  for (const [k, v] of Object.entries(vocabulary.words))
    strings[`status.${k}`] = { extractionState: "manual", localizations: { en: unit(v) } };
  for (const [k, v] of Object.entries(vocabulary.plurals)) {
    const plural = Object.fromEntries(
      icuForms(v, "%lld").map(([form, text]) => [form, unit(text)]),
    );
    strings[k] = { extractionState: "manual", localizations: { en: { variations: { plural } } } };
  }
  const sorted = Object.fromEntries(
    Object.keys(strings)
      .sort()
      .map((k) => [k, strings[k]]),
  );
  return (
    JSON.stringify({ sourceLanguage: "en", strings: sorted, version: "1.0" }, null, 2).replaceAll(
      '": ',
      '" : ',
    ) + "\n"
  );
}

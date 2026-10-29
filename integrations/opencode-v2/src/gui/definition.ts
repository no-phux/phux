import { Schema } from "effect";
import { Plugin, Store } from "@opencode/plugin/gui";

import { en } from "./i18n.js";

export const Prefs = Schema.Struct({
  notifyBlocked: Schema.Boolean,
  notifyFinished: Schema.Boolean,
  openSpawned: Schema.Boolean,
  fitWidth: Schema.Boolean,
});

export type Prefs = typeof Prefs.Type;

export const definition = Plugin.define({
  stores: {
    prefs: Store.global(Prefs, { notifyBlocked: true, notifyFinished: true, openSpawned: true, fitWidth: true }),
  },
  i18n: { en },
});

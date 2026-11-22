import { Plugin } from "@opencode/plugin/gui";

import { definition } from "./gui/definition.js";
import { setup } from "./gui/window.js";

/**
 * OpenCode Desktop and web: phux panes beside the conversation. Window-only;
 * the server plugin owns the phux CLI and answers through its RPC.
 */
export default Plugin.define({ ...definition, setup });

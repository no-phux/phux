import { registerHooks } from 'node:module';
import { pathToFileURL } from 'node:url';
import { resolve, isAbsolute, join } from 'node:path';
import { Console } from 'node:console';

const provider = process.env.PHUX_NATIVE_PROVIDER;
const activation = process.env.PHUX_NATIVE_ACTIVATION;
const cwd = process.env.PHUX_NATIVE_CWD;
const runtime = process.env.PHUX_NATIVE_SDK_RUNTIME;
const resumeSessionId = process.env.PHUX_NATIVE_SESSION_ID;
let command: unknown;
try { command = JSON.parse(process.env.PHUX_NATIVE_COMMAND ?? '[]'); } catch { command = null; }
const providers = ['pi', 'claude', 'opencode', 'codex', 'amp'];
if (!provider || !providers.includes(provider) || !activation || activation.length > 256 || !cwd || !isAbsolute(cwd) || !runtime || !isAbsolute(runtime) || !Array.isArray(command) || !command.every((part): part is string => typeof part === 'string') || command.length > 64) {
  process.stdout.write(JSON.stringify({ type: 'failed', message: 'Invalid native SDK startup configuration' }) + '\n');
  process.exitCode = 1;
} else {
  const startup = { provider, activation, cwd, runtime: resolve(runtime), command: command as string[], ...(resumeSessionId ? { resumeSessionId } : {}) };
  // Provider extensions may log. Protocol stdout belongs exclusively to phux.
  const writeProtocol = process.stdout.write.bind(process.stdout);
  process.stdout.write = process.stderr.write.bind(process.stderr);
  globalThis.console = new Console({ stdout: process.stderr, stderr: process.stderr });
  for (const key of Object.keys(process.env)) {
    if (key.startsWith('PHUX_NATIVE_') || key.startsWith('PHUX_CONTROLLER_') || key === 'PHUX_BINDING_PROOF' || key === 'AMP_DEBUG') delete process.env[key];
  }
  process.env.PHUX_AGENT_REPORTING = '0';
  const ownUrl = import.meta.url;
  const runtimeUrl = pathToFileURL(join(startup.runtime, 'package.json')).href;
  registerHooks({
    resolve(specifier, context, nextResolve) {
      // Rewrite only own-code external imports. SDK-internal imports MUST keep
      // their own parent so separately locked peer versions resolve correctly.
      if (context.parentURL === ownUrl && !specifier.startsWith('.') && !specifier.startsWith('/') && !specifier.startsWith('node:') && !specifier.startsWith('file:')) {
        return nextResolve(specifier, { ...context, parentURL: runtimeUrl });
      }
      return nextResolve(specifier, context);
    },
  });
  // The isolated runtime hook and credential stripping must precede all SDK
  // evaluation. Bundling keeps this own-code lazy initialization boundary.
  try {
    const { runBridge } = await import('./bridge.js');
    await runBridge(startup, writeProtocol);
  } catch {
    writeProtocol(JSON.stringify({ type: 'failed', message: 'Native SDK runtime could not be loaded from its reviewed isolated installation' }) + '\n');
    process.exitCode = 1;
  }
}

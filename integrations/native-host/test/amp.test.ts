import assert from 'node:assert/strict';
import test from 'node:test';
import { AmpTranscript } from '../src/amp.js';
import type { AdapterContext, NativeUpdate } from '../src/types.js';

test('Amp projects actual message/tool IDs and never creates approval callbacks', () => {
  const updates: NativeUpdate[] = [];
  const identities: string[] = [];
  const context: AdapterContext = { provider: 'amp', activation: 'a', cwd: '/tmp', command: [], runtime: '/tmp/private', env: {}, emit: update => { updates.push(update); }, identity: id => { identities.push(id); }, turn: () => {}, connection: () => {}, question: async () => { throw new Error('Amp must not synthesize permission questions'); }, closeQuestions: () => {} };
  const projection = new AmpTranscript(context);
  projection.emit({ type: 'system', subtype: 'init', session_id: 'T-real', cwd: '/tmp', tools: ['Read'], mcp_servers: [] }, 'request-1');
  projection.emit({ type: 'assistant', session_id: 'T-real', parent_tool_use_id: null, message: { id: 'msg-real', role: 'assistant', type: 'message', model: 'provider-model', stop_reason: 'tool_use', stop_sequence: null, content: [{ type: 'text', text: 'Reading' }, { type: 'tool_use', id: 'tool-real', name: 'Read', input: { path: '/tmp/file' } }] } }, 'request-1');
  projection.emit({ type: 'user', session_id: 'T-real', parent_tool_use_id: null, message: { role: 'user', content: [{ type: 'tool_result', tool_use_id: 'tool-real', content: 'private file content', is_error: false }] } }, 'request-1');
  projection.emit({ type: 'result', subtype: 'success', is_error: false, session_id: 'T-real', result: 'Finished', duration_ms: 2, num_turns: 1, permission_denials: ['Shell'] }, 'request-1');
  assert.equal(projection.sessionId, 'T-real');
  assert.ok(identities.every(id => id === 'T-real'));
  const transcripts = updates.flatMap(update => update.type === 'transcript' ? [update.entry] : []);
  assert.equal(transcripts[0].id, 'msg-real');
  assert.equal(transcripts[1].tool?.call_id, 'tool-real');
  assert.equal(transcripts[1].tool?.status, 'running');
  assert.equal(transcripts[2].tool?.call_id, 'tool-real');
  assert.equal(transcripts[2].tool?.status, 'ok');
  assert.equal(transcripts[2].tool?.output, '');
  assert.ok(updates.some(update => update.type === 'notice' && update.text.includes('does not expose')));
  assert.ok(!updates.some(update => update.type === 'question_opened'));
  assert.deepEqual(updates.at(-1), { type: 'state', state: 'done' });
  assert.throws(() => projection.emit({ type: 'system', subtype: 'init', session_id: 'T-different', cwd: '/tmp', tools: [], mcp_servers: [] }, 'request-1'), /identity/);
});

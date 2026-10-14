import type { AnySchema, ValidateFunction } from 'ajv';
import type { Json, NativeQuestion } from './types.js';

export interface PendingQuestion {
  question: NativeQuestion;
  settled: boolean;
  validate?: ValidateFunction;
  resolve(value: Json): void;
  reject(reason: Error): void;
  dispose(): void;
}
export class QuestionRegistry {
  readonly pending = new Map<string, PendingQuestion>();
  constructor(private readonly compile: (schema: AnySchema) => ValidateFunction, private readonly publish: (question: NativeQuestion) => void, private readonly close: (id: string, reason: string) => void) {}

  open(question: NativeQuestion, signal?: AbortSignal): Promise<Json> {
    if (signal?.aborted) return Promise.reject(new Error('Provider question cancelled'));
    if (this.pending.get(question.id)?.settled === false) return Promise.reject(new Error('Duplicate active provider question ID'));
    const totalBytes = [...this.pending.values()].filter(item => item.question.id !== question.id).reduce((sum, item) => sum + Buffer.byteLength(JSON.stringify(item.question)), 0) + Buffer.byteLength(JSON.stringify(question));
    if ((!this.pending.has(question.id) && this.pending.size >= 32) || totalBytes > 12 * 1024 || question.id.length > 256) return Promise.reject(new Error('Native question bounds exceeded'));
    let validate: ValidateFunction | undefined;
    if (question.kind === 'form' && question.fields !== null && typeof question.fields === 'object' && !Array.isArray(question.fields) && 'properties' in question.fields) {
      try { validate = this.compile(question.fields as AnySchema); }
      catch { return Promise.reject(new Error('Provider form schema is not supported by the native validator')); }
    }
    this.pending.get(question.id)?.dispose();
    return new Promise<Json>((resolve, reject) => {
      const cancelled = (): void => {
        this.pending.delete(question.id);
        reject(new Error('Provider question cancelled'));
        this.close(question.id, 'provider_cancelled');
      };
      const item: PendingQuestion = { question, settled: false, validate, resolve, reject, dispose: () => signal?.removeEventListener('abort', cancelled) };
      this.pending.set(question.id, item);
      signal?.addEventListener('abort', cancelled, { once: true });
      this.publish(question);
    });
  }

  answer(id: string, response: Json): { accepted: boolean; detail: string } {
    const item = this.pending.get(id);
    if (!item || item.settled) return { accepted: false, detail: 'Question is stale or already has a responder' };
    if (response === null || typeof response !== 'object' || Array.isArray(response)) return { accepted: false, detail: 'Answer must be a typed object' };
    const q = item.question;
    let valid = false;
    if (response.cancelled === true) valid = true;
    else if (q.kind === 'confirm') valid = typeof response.confirmed === 'boolean';
    else if (q.kind === 'input' || q.kind === 'editor') valid = typeof response.text === 'string';
    else if (q.kind === 'select' || q.kind === 'permission') valid = typeof response.option_id === 'string' && q.options.some(option => option.id === response.option_id);
    else if (q.kind === 'form') valid = response.values !== null && typeof response.values === 'object' && !Array.isArray(response.values) && (!item.validate || item.validate(response.values));
    else if (q.kind === 'ask' && Array.isArray(response.answers)) {
      const seen = new Set<string>();
      valid = response.answers.length === q.questions.length && response.answers.every(answer => {
        if (answer === null || typeof answer !== 'object' || Array.isArray(answer) || typeof answer.id !== 'string' || seen.has(answer.id)) return false;
        seen.add(answer.id);
        const question = q.questions.find(question => question.id === answer.id);
        if (!question || !Array.isArray(answer.selected_options) || (!question.multi && answer.selected_options.length > 1)) return false;
        const selected = new Set<string>();
        return answer.selected_options.every(option => {
          if (typeof option !== 'string' || selected.has(option) || !question.options.some(candidate => candidate.id === option)) return false;
          selected.add(option);
          return true;
        }) && (answer.custom_input === undefined || typeof answer.custom_input === 'string');
      });
    }
    if (!valid) return { accepted: false, detail: 'Answer does not match the current provider question' };
    item.settled = true;
    // Remain visible/reserved until a factual SDK/API response closes it. This
    // resolution means submission to a callback, not provider consumption.
    item.resolve(response);
    return { accepted: true, detail: 'Answer queued to the waiting SDK callback; provider consumption is not yet confirmed' };
  }

  closed(id: string): void {
    const item = this.pending.get(id);
    item?.dispose();
    if (item && !item.settled) item.reject(new Error('Provider closed question'));
    this.pending.delete(id);
  }

  closeAll(reason: string): void {
    for (const [id, item] of this.pending) {
      item.dispose();
      if (!item.settled) item.reject(new Error('Native controller closed'));
      this.close(id, reason);
    }
    this.pending.clear();
  }
}

import type { TranscriptEntry, TranscriptTool } from '../../runtime/src/transcript.js';
export type { TranscriptEntry, TranscriptTool };
export type Json = null | boolean | number | string | Json[] | { [key: string]: Json };
export interface NativeOption { id: string; label: string; description?: string }
export interface NativeQuestionItem { id: string; question: string; header: string; options: NativeOption[]; multi: boolean; recommended?: string }
export interface NativeQuestion { id: string; kind: 'confirm' | 'select' | 'input' | 'editor' | 'ask' | 'permission' | 'form'; title: string; message: string; options: NativeOption[]; questions: NativeQuestionItem[]; fields: Json }
export interface NativeCapabilities { prompt: boolean; interrupt: boolean; questions: boolean; follow_up: boolean }
export type NativeIntent = { type: 'prompt'; text: string } | { type: 'interrupt'; turn_id: string } | { type: 'answer'; question_id: string; response: Json };
export interface NativeCommand { operation_id: string; activation: string; intent: NativeIntent }
export interface NativeCommandResult { operation_id: string; disposition: 'admitted' | 'queued' | 'completed' | 'refused' | 'unknown'; detail?: string }
export interface NativeView { activation: string; backend: string; capabilities: NativeCapabilities; connected: boolean; turn_id?: string; provider_session_id?: string; questions: NativeQuestion[] }
export type NativeUpdate = { type: 'view'; view: NativeView } | { type: 'question_opened'; question: NativeQuestion } | { type: 'question_closed'; id: string; reason: string } | { type: 'state'; state: 'working' | 'blocked' | 'done'; turn_id?: string } | { type: 'transcript'; entry: TranscriptEntry } | { type: 'notice'; level: 'info' | 'warning' | 'error'; text: string };
export interface AdapterContext {
  provider: string;
  activation: string;
  cwd: string;
  command: string[];
  runtime: string;
  env: NodeJS.ProcessEnv;
  resumeSessionId?: string;
  connection(connected: boolean): void;
  emit(update: NativeUpdate): void;
  result(result: NativeCommandResult): void;
  identity(id: string): void;
  turn(id: string | undefined): void;
  question(question: NativeQuestion, signal?: AbortSignal): Promise<Json>;
  closeQuestions(reason: string): void;
}
export interface NativeAdapter {
  capabilities: NativeCapabilities;
  providerSessionId?: string;
  prompt(text: string, operationId: string): Promise<{ disposition: NativeCommandResult['disposition']; detail?: string }>;
  interrupt(turnId: string): Promise<{ disposition: NativeCommandResult['disposition']; detail?: string }>;
  close(): Promise<void>;
}
export interface AdapterModule { createAdapter(context: AdapterContext): Promise<NativeAdapter> }

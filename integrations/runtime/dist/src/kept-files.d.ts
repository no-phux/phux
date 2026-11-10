/**
 * Files a tool hands the agent instead of inlining (an oversized tree, a
 * frame too large to attach) live in one private directory per user. Each
 * new file prunes the rest: none outlives the retention window, and at most
 * MAX_KEPT_FILES exist at once.
 */
export declare const KEPT_FILE_RETENTION_MS: number;
export declare const MAX_KEPT_FILES = 32;
export declare const KEPT_FILES_NOTE = "Saved files are kept about 10 minutes, at most 32 at a time, in a private temporary directory.";
export declare function keptFilesDirectory(): string;
/** A fresh path in the private directory, after pruning what retention no longer allows. */
export declare function keptFilePath(prefix: string, extension: string, now?: number): Promise<string>;

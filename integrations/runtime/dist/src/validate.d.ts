/** Primitive validators shared by the CLI result parsers. */
export declare class SchemaValidationError extends Error {
    readonly path: string;
    constructor(path: string, expectation: string);
}
export type RecordValue = Record<string, unknown>;
export declare function record(value: unknown, path: string): RecordValue;
export declare function string(value: unknown, path: string): string;
export declare function boolean(value: unknown, path: string): boolean;
export declare function nullableString(value: unknown, path: string): string | null;
export declare function numberInRange(value: unknown, path: string, min: number, max: number): number;
export declare function oneOf<const T extends readonly string[]>(value: unknown, path: string, values: T): T[number];
export declare function integer(value: unknown, path: string, min: number, max?: number): number;
export declare function strings(value: unknown, path: string): string[];

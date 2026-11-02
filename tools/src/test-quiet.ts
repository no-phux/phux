// Tests drive failure paths on purpose, and the code under test reports them
// the way it reports them to a person. A passing run should not print
// `phux: error:`, so those lines stop here. Bun's own reporter does not go
// through process.stderr, and any other output still does.
const noise = /^(phux: |  hint: |Full validation: )/;
const write = process.stderr.write.bind(process.stderr);
process.stderr.write = ((chunk: string | Uint8Array, ...rest: never[]) =>
  noise.test(String(chunk)) || write(chunk, ...rest)) as typeof process.stderr.write;
const error = console.error.bind(console);
console.error = (...args: unknown[]) => {
  if (!noise.test(String(args[0]))) error(...args);
};

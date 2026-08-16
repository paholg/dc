// Ajv setup for `just check-schema`: strict mode rejects keywords and formats
// it doesn't know, so register the ones the schema uses deliberately — the
// tombi vendor extension, and the integer formats schemars emits.
module.exports = (ajv) => {
  ajv.addKeyword("x-tombi-toml-version");

  for (const [format, max] of [
    ["uint16", 65535],
    ["uint64", Number.MAX_SAFE_INTEGER],
  ]) {
    ajv.addFormat(format, {
      type: "number",
      validate: (n) => Number.isInteger(n) && n >= 0 && n <= max,
    });
  }
};

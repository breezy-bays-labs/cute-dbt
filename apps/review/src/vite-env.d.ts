/// <reference types="vite/client" />

// JSON fixture modules are imported as default-exported `unknown` (validated by
// the Zod gate at load — never trusted by shape).
declare module "*.json" {
  const value: unknown;
  export default value;
}

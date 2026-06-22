import { createRequire } from "node:module";

const require = createRequire(import.meta.url);
/* eslint-disable @typescript-eslint/no-require-imports, @typescript-eslint/no-explicit-any */
export const Decimal: any = require("decimal.js");

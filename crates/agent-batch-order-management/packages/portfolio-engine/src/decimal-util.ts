import { Decimal } from "./decimal-cjs.js";

/** Приводим к экземпляру Decimal (`Decimal.isDecimal` при наличии). Конструктор — any (CJS), поэтому возвращаем any. */
export function D(value: unknown): any {
  if ((Decimal as { isDecimal?: (x: unknown) => boolean }).isDecimal?.(value)) return value;
  return new Decimal(String(value));
}

export const toDecimal = D;

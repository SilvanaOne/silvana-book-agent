"use client";

import type { ReactNode } from "react";

/**
 * Информационная подсказка «(i)» в стиле Silvana Book.
 * Показывает текст по hover/focus без JS — через CSS (`:hover`, `:focus-within`).
 *
 * Поведение:
 * - Серверный компонент: возвращает `<span>` со вложенной кнопкой и пузырём.
 * - Кнопка с `tabIndex=0` доступна с клавиатуры (Tab → подсказка появляется на focus).
 * - `aria-label` на кнопке + дублирование текста как `title` (нативный fallback браузера).
 *
 * Размещение пузыря: по центру над иконкой; при необходимости можно задать `placement`
 * (`top` по умолчанию или `bottom`) или сместить пузырь к правому краю иконки через `align="end"`.
 */
export type InfoTipProps = Readonly<{
  text: string;
  label?: string;
  placement?: "top" | "bottom";
  align?: "center" | "start" | "end";
  children?: ReactNode;
}>;

export function InfoTip({ text, label, placement = "top", align = "center", children }: InfoTipProps) {
  const cls = [
    "silv-infotip",
    `silv-infotip--${placement}`,
    `silv-infotip--align-${align}`,
  ].join(" ");

  return (
    <span className={cls}>
      {children}
      <button
        type="button"
        className="silv-infotip__icon"
        aria-label={label ?? "More info"}
        title={text}
      >
        <span aria-hidden>i</span>
      </button>
      <span role="tooltip" className="silv-infotip__bubble">
        {text}
      </span>
    </span>
  );
}

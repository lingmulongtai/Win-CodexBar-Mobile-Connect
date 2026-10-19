import type React from "react";
import { createContext, useContext, useId } from "react";

// A Field's label and description ids, so the control inside it gets an
// accessible name without every call site passing `ariaLabel`.
const FieldContext = createContext<{ labelId: string; descId?: string } | null>(null);

/** aria props naming a control after its enclosing Field; an explicit ariaLabel wins. */
export function useFieldAria(ariaLabel?: string) {
  const field = useContext(FieldContext);
  if (ariaLabel || !field) return { "aria-label": ariaLabel };
  return { "aria-labelledby": field.labelId, "aria-describedby": field.descId };
}

// ── tiny reusable controls ──────────────────────────────────────────

export function Toggle({
  checked,
  onChange,
  label,
  ariaLabel,
  disabled,
  variant = "checkbox",
}: {
  checked: boolean;
  onChange: (v: boolean) => void;
  label?: string;
  ariaLabel?: string;
  disabled?: boolean;
  /** "switch" draws the macOS-style switch used by the grouped settings panes. */
  variant?: "checkbox" | "switch";
}) {
  const fieldAria = useFieldAria(ariaLabel);
  const input = (
    <input
      type="checkbox"
      className={variant === "switch" ? "toggle toggle--switch" : "toggle"}
      checked={checked}
      {...(label ? { "aria-label": ariaLabel } : fieldAria)}
      disabled={disabled}
      onChange={(e) => onChange(e.target.checked)}
    />
  );
  if (label) {
    return (
      <label className={`toggle-label ${disabled ? "toggle-label--disabled" : ""}`}>
        {input}
        <span>{label}</span>
      </label>
    );
  }
  return input;
}

export function Select({
  value,
  options,
  onChange,
  disabled,
  ariaLabel,
  minWidth,
  variant = "box",
}: {
  value: string;
  options: { value: string; label: string; disabled?: boolean }[];
  onChange: (v: string) => void;
  disabled?: boolean;
  ariaLabel?: string;
  minWidth?: number;
  /** "menu" draws a borderless "Value ⌄" pop-up button like the macOS panes. */
  variant?: "box" | "menu";
}) {
  const fieldAria = useFieldAria(ariaLabel);
  return (
    <select
      className={variant === "menu" ? "select select--menu" : "select"}
      style={{ minWidth }}
      value={value}
      disabled={disabled}
      {...fieldAria}
      onChange={(e) => onChange(e.target.value)}
    >
      {options.map((o) => (
        <option key={o.value} value={o.value} disabled={o.disabled}>
          {o.label}
        </option>
      ))}
    </select>
  );
}

export function NumberInput({
  value,
  min,
  max,
  step,
  onChange,
  disabled,
  ariaLabel,
}: {
  value: number;
  min?: number;
  max?: number;
  step?: number;
  onChange: (v: number) => void;
  disabled?: boolean;
  ariaLabel?: string;
}) {
  const fieldAria = useFieldAria(ariaLabel);
  return (
    <input
      type="number"
      className="number-input"
      value={value}
      min={min}
      max={max}
      step={step}
      disabled={disabled}
      {...fieldAria}
      onChange={(e) => {
        const raw = e.target.value;
        if (raw === "") return;
        const n = Number(raw);
        if (!Number.isNaN(n)) onChange(n);
      }}
    />
  );
}

// ── field row ────────────────────────────────────────────────────────

export function Field({
  label,
  description,
  children,
  leading,
}: {
  label: string;
  description?: string;
  children: React.ReactNode;
  leading?: boolean;
}) {
  const id = useId();
  const ids = { labelId: `${id}-label`, descId: description ? `${id}-desc` : undefined };
  const control = (
    <div className="settings-field__control">
      <FieldContext.Provider value={ids}>{children}</FieldContext.Provider>
    </div>
  );
  return (
    <div className={`settings-field${leading ? " settings-field--leading" : ""}`}>
      {leading && control}
      <div className="settings-field__text">
        <span id={ids.labelId} className="settings-field__label">{label}</span>
        {description && (
          <span id={ids.descId} className="settings-field__desc">{description}</span>
        )}
      </div>
      {!leading && control}
    </div>
  );
}

// ── grouped section (macOS grouped Form look) ───────────────────────

/** A bold section title, a rounded box of rows with dividers, and an optional footer. */
export function SettingsGroup({
  title,
  footer,
  children,
  className,
}: {
  title?: string;
  footer?: React.ReactNode;
  children: React.ReactNode;
  /** Extra classes on the `section` (e.g. a dimmed or provider-detail variant). */
  className?: string;
}) {
  const id = useId();
  return (
    <section
      className={className ? `settings-group ${className}` : "settings-group"}
      aria-labelledby={title ? `${id}-title` : undefined}
    >
      {title && (
        <h3 id={`${id}-title`} className="settings-group__title">
          {title}
        </h3>
      )}
      <div className="settings-group__box">{children}</div>
      {footer && <div className="settings-group__footer">{footer}</div>}
    </section>
  );
}

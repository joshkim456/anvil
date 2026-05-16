import { useEffect, useRef, useState } from "react";

export interface Opt {
  value: string;
  label: string;
}

function useClickOutside(
  ref: React.RefObject<HTMLDivElement>,
  onClose: () => void,
) {
  useEffect(() => {
    function handler(e: MouseEvent) {
      if (ref.current && !ref.current.contains(e.target as Node)) onClose();
    }
    document.addEventListener("mousedown", handler);
    return () => document.removeEventListener("mousedown", handler);
  }, [ref, onClose]);
}

/** Single-select, fully custom-rendered (no native popup). */
export function Dropdown({
  value,
  options,
  onChange,
}: {
  value: string;
  options: Opt[];
  onChange: (v: string) => void;
}) {
  const [open, setOpen] = useState(false);
  const ref = useRef<HTMLDivElement>(null);
  useClickOutside(ref, () => setOpen(false));
  const cur = options.find((o) => o.value === value) ?? options[0];

  return (
    <div className="dd" ref={ref}>
      <button
        className="dd-trigger"
        onClick={() => setOpen((o) => !o)}
        type="button"
      >
        <span>{cur?.label}</span>
        <span className="dd-chev" aria-hidden>
          ▾
        </span>
      </button>
      {open && (
        <div className="dd-menu">
          {options.map((o) => (
            <button
              key={o.value}
              type="button"
              className={`dd-item ${o.value === value ? "sel" : ""}`}
              onClick={() => {
                onChange(o.value);
                setOpen(false);
              }}
            >
              <span className="dd-mark">{o.value === value ? "✓" : ""}</span>
              {o.label}
            </button>
          ))}
        </div>
      )}
    </div>
  );
}

/** Multi-select checklist; stays open while toggling. */
export function MultiSelect({
  selected,
  options,
  onChange,
  placeholder,
}: {
  selected: string[];
  options: Opt[];
  onChange: (v: string[]) => void;
  placeholder: string;
}) {
  const [open, setOpen] = useState(false);
  const ref = useRef<HTMLDivElement>(null);
  useClickOutside(ref, () => setOpen(false));

  const label =
    selected.length === 0
      ? placeholder
      : selected.length === 1
        ? (options.find((o) => o.value === selected[0])?.label ?? placeholder)
        : `${selected.length} selected`;

  function toggle(v: string) {
    onChange(
      selected.includes(v)
        ? selected.filter((x) => x !== v)
        : [...selected, v],
    );
  }

  return (
    <div className="dd" ref={ref}>
      <button
        className="dd-trigger"
        onClick={() => setOpen((o) => !o)}
        type="button"
      >
        <span>{label}</span>
        <span className="dd-chev" aria-hidden>
          ▾
        </span>
      </button>
      {open && (
        <div className="dd-menu">
          <button
            type="button"
            className={`dd-item ${selected.length === 0 ? "sel" : ""}`}
            onClick={() => onChange([])}
          >
            <span className="dd-box">{selected.length === 0 ? "✓" : ""}</span>
            {placeholder}
          </button>
          {options.map((o) => {
            const on = selected.includes(o.value);
            return (
              <button
                key={o.value}
                type="button"
                className={`dd-item ${on ? "sel" : ""}`}
                onClick={() => toggle(o.value)}
              >
                <span className="dd-box">{on ? "✓" : ""}</span>
                {o.label}
              </button>
            );
          })}
        </div>
      )}
    </div>
  );
}

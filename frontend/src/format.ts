// Formatting of decimal strings from the API. Prices, quantities and money
// arrive as decimal strings and are rounded here with BigInt arithmetic, never
// through binary floating point (INV-13).

export type DecimalString = string;

function split(value: DecimalString): { negative: boolean; digits: bigint; scale: number } | null {
  const match = /^(-)?(\d+)(?:\.(\d+))?$/.exec(value.trim());
  if (!match) return null;
  const fraction = match[3] ?? "";
  return {
    negative: match[1] === "-",
    digits: BigInt((match[2] ?? "0") + fraction),
    scale: fraction.length,
  };
}

/** Rounds half away from zero to `dp` decimals. Returns null for non-decimals. */
export function roundDecimal(value: DecimalString, dp: number): string | null {
  const parts = split(value);
  if (!parts) return null;
  let { digits } = parts;
  if (parts.scale > dp) {
    const divisor = 10n ** BigInt(parts.scale - dp);
    const quotient = digits / divisor;
    const remainder = digits % divisor;
    digits = remainder * 2n >= divisor ? quotient + 1n : quotient;
  } else {
    digits = digits * 10n ** BigInt(dp - parts.scale);
  }
  const text = digits.toString().padStart(dp + 1, "0");
  const intPart = dp > 0 ? text.slice(0, -dp) : text;
  const fracPart = dp > 0 ? text.slice(-dp) : "";
  const isZero = digits === 0n;
  const sign = parts.negative && !isZero ? "-" : "";
  return sign + intPart + (dp > 0 ? "." + fracPart : "");
}

/** Groups an integer string the Indian way: 12,48,200. */
export function groupIndian(integer: string): string {
  if (integer.length <= 3) return integer;
  const last3 = integer.slice(-3);
  const rest = integer.slice(0, -3);
  return rest.replace(/\B(?=(\d{2})+(?!\d))/g, ",") + "," + last3;
}

/** Rounds and groups: "1248200.456" → "12,48,200.46". */
export function formatNumber(value: DecimalString | null | undefined, dp = 2): string {
  if (value === null || value === undefined) return "—";
  const rounded = roundDecimal(value, dp);
  if (rounded === null) return "—";
  const negative = rounded.startsWith("-");
  const body = negative ? rounded.slice(1) : rounded;
  const [int = "0", frac] = body.split(".");
  return (negative ? "-" : "") + groupIndian(int) + (frac !== undefined ? "." + frac : "");
}

/** A fraction as a percentage: "0.0213" → "2.13%". */
export function formatPercent(value: DecimalString | null | undefined, dp = 2): string {
  if (value === null || value === undefined) return "—";
  const parts = split(value);
  if (!parts) return "—";
  // Multiply by 100 by shifting the scale.
  const shifted = (parts.negative ? "-" : "") + insertPoint(parts.digits.toString(), parts.scale - 2);
  const rounded = roundDecimal(shifted, dp);
  return rounded === null ? "—" : rounded + "%";
}

function insertPoint(digits: string, scale: number): string {
  if (scale <= 0) return digits + "0".repeat(-scale);
  const padded = digits.padStart(scale + 1, "0");
  return padded.slice(0, -scale) + "." + padded.slice(-scale);
}

/** A timestamp in IST (spec §6.1). */
export function formatIst(iso: string): string {
  const date = new Date(iso);
  if (Number.isNaN(date.getTime())) return iso;
  return new Intl.DateTimeFormat("en-IN", {
    timeZone: "Asia/Kolkata",
    year: "numeric",
    month: "short",
    day: "2-digit",
    hour: "2-digit",
    minute: "2-digit",
    hour12: false,
  }).format(date) + " IST";
}

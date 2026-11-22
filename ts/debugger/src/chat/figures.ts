// Figures as the chat view and the usage page show them: money in US
// dollars, as CAF counts it, and tokens and shares, short.

/** US dollars, to the cent: `$1.84`. Anything above nothing but under a cent shows as `<$0.01`, so it isn't read as none. */
export function usd(amount: number): string {
  if (amount > 0 && amount < 0.005) return `<${dollars.format(0.01)}`
  return dollars.format(amount)
}

/** US dollars per million tokens, as prices are: `$0.30`, `$15`. */
export function rate(amount: number): string {
  return Number.isInteger(amount) ? wholeDollars.format(amount) : fineDollars.format(amount)
}

/** A count of tokens, short: `950`, `195K`, `2.1M`. */
export function tokens(n: number): string {
  return compact.format(n)
}

/** A count in full, for a tooltip: `2,104,391`. */
export function count(n: number): string {
  return whole.format(n)
}

/**
 * A share of a whole, as a percentage: `33%`, `4.2%` under a tenth (as
 * it rounds, so 0.0999 is `10%`, as 0.1 is), or `<0.1%` for a sliver.
 */
export function percent(share: number): string {
  if (!Number.isFinite(share)) return '–'
  const points = share * 100
  if (points > 0 && points < 0.05) return '<0.1%'
  return `${points.toFixed(points > 0 && Math.round(points * 10) < 100 ? 1 : 0)}%`
}

const dollars = new Intl.NumberFormat(undefined, { style: 'currency', currency: 'USD' })
const wholeDollars = new Intl.NumberFormat(undefined, { style: 'currency', currency: 'USD', maximumFractionDigits: 0 })
const fineDollars = new Intl.NumberFormat(undefined, { style: 'currency', currency: 'USD', maximumFractionDigits: 4 })
const compact = new Intl.NumberFormat(undefined, { notation: 'compact', maximumFractionDigits: 1 })
const whole = new Intl.NumberFormat()

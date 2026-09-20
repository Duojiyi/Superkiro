export function pointsToMicro(value: string): number {
  if (!/^\d+(\.\d{1,6})?$/.test(value)) throw new Error('积分价格必须为非负数，最多六位小数');
  const [whole, fraction = ''] = value.split('.');
  const exact = BigInt(whole) * 1_000_000n + BigInt(fraction.padEnd(6, '0'));
  const micro = Number(exact);
  if (exact > BigInt(Number.MAX_SAFE_INTEGER)) throw new Error('积分价格超出安全范围');
  return micro;
}

export function adjustmentPointsToMicro(value:string):number {
  const text=value.trim(),negative=text.startsWith('-');
  const magnitude=pointsToMicro(negative?text.slice(1):text);
  if(magnitude===0||magnitude>1_000_000_000_000)throw new Error('调账须为非零、最多六位小数，范围为 ±1,000,000 积分');
  return negative?-magnitude:magnitude;
}

export type PriceUnit = 'million' | 'thousand';

// The API always stores integer microcredits per million tokens.
export function priceToMicroPerMillion(value: string, unit: PriceUnit): number {
  const digits = unit === 'million' ? 6 : 9;
  if (!new RegExp('^\\d+(\\.\\d{1,' + digits + '})?$').test(value))
    throw new Error('售价须为非负十进制数，当前单位最多 ' + digits + ' 位小数');
  const [whole, fraction = ''] = value.split('.');
  const exact = BigInt(whole) * 10n ** BigInt(digits) + BigInt(fraction.padEnd(digits, '0'));
  if (exact > BigInt(Number.MAX_SAFE_INTEGER)) throw new Error('售价超出安全整数范围');
  return Number(exact);
}

export function formatMicroPrice(value: number, unit: PriceUnit = 'million'): string {
  if (!Number.isSafeInteger(value) || value < 0) throw new Error('微积分价格必须为非负安全整数');
  const digits = unit === 'million' ? 6 : 9, scale = 10n ** BigInt(digits), exact = BigInt(value);
  const fraction = (exact % scale).toString().padStart(digits, '0').replace(/0+$/, '');
  return (exact / scale).toString() + (fraction ? '.' + fraction : '');
}

// Match RateCardVersion::calculate_charge: input, output, cache creation, cache read.
// Billing currently uses f64 arithmetic and rounds only the final charge upward.
export function previewFixedCharge(rates: number[], tokens: string[], multipliers: number[]): number {
  if (rates.length !== 4 || tokens.length !== 4 || multipliers.length !== 3)
    throw new Error('扣费预览需要四类用量和三个倍率');
  let base = 0;
  rates.forEach((rate, index) => {
    if (!Number.isSafeInteger(rate) || rate < 0 || !/^\d+$/.test(tokens[index]))
      throw new Error('价格及 Token 用量必须为非负整数');
    const count = Number(tokens[index]);
    if (!Number.isSafeInteger(count)) throw new Error('Token 用量超出安全范围');
    const product = count * rate;
    if (!Number.isSafeInteger(product)) throw new Error('用量与单价乘积超出精确预览范围，请减小示例用量');
    base += product / 1_000_000;
  });
  if (multipliers.some(value => !Number.isFinite(value) || value < 0)) throw new Error('倍率须为非负有限数');
  const charge = Math.ceil(base * (multipliers[0] * multipliers[1] * multipliers[2]));
  if (!Number.isSafeInteger(charge) || charge < 0) throw new Error('扣费结果超出安全预览范围');
  return charge;
}

export function pointsToMicro(value: string): number {
  if (!/^\d+(\.\d{1,6})?$/.test(value)) throw new Error('积分价格必须为非负数，最多六位小数');
  const [whole, fraction = ''] = value.split('.');
  const micro = Number(whole) * 1_000_000 + Number(fraction.padEnd(6, '0'));
  if (!Number.isSafeInteger(micro)) throw new Error('积分价格超出安全范围');
  return micro;
}

export function adjustmentPointsToMicro(value:string):number {
  const text=value.trim(),negative=text.startsWith('-');
  const magnitude=pointsToMicro(negative?text.slice(1):text);
  if(magnitude===0||magnitude>1_000_000_000_000)throw new Error('调账须为非零、最多六位小数，范围为 ±1,000,000 积分');
  return negative?-magnitude:magnitude;
}

export function pointsToMicro(value: string): number {
  if (!/^\d+(\.\d{1,6})?$/.test(value)) throw new Error('积分价格必须为非负数，最多六位小数');
  const [whole, fraction = ''] = value.split('.');
  const micro = Number(whole) * 1_000_000 + Number(fraction.padEnd(6, '0'));
  if (!Number.isSafeInteger(micro)) throw new Error('积分价格超出安全范围');
  return micro;
}

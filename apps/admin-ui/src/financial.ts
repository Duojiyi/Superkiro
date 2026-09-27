import type {AdminFinancials} from './api';
export function parseFinancialSettings(faceValue:string,exchangeRate:string){
  const credit_face_value_cny=Number(faceValue),usd_cny_rate=Number(exchangeRate);
  if(!faceValue.trim()||!exchangeRate.trim()||![credit_face_value_cny,usd_cny_rate].every(v=>Number.isFinite(v)&&v>0&&v<=1000))throw new Error('积分面值与汇率须大于 0、至多 1000');
  return {credit_face_value_cny,usd_cny_rate};
}
/**
 * The requests left out of the margin, in words: those settled at an estimated cost (for want of the
 * serving route's own) and those whose route has no cost at all.
 */
export function uncostedText(uncosted:number,estimated=0):string{
  const guessed=Math.min(Math.max(0,estimated),uncosted),unset=uncosted-guessed;
  if(!guessed)return `另有 ${uncosted.toLocaleString('en-US')} 次未设成本`;
  if(!unset)return `另有 ${uncosted.toLocaleString('en-US')} 次成本为估算，没算进毛利`;
  return `另有 ${uncosted.toLocaleString('en-US')} 次没算进毛利：成本为估算 ${guessed.toLocaleString('en-US')} 次、未设成本 ${unset.toLocaleString('en-US')} 次`;
}
export function financialEstimates(data:AdminFinancials|null){
  const e=data?.estimates;
  if(data?.basis!=='retained_usage_ledger_estimate_not_cash_revenue'||!e||e.retainedLedgerOnly!==true||![e.costedRequests,e.uncostedRequests].every(v=>Number.isSafeInteger(v)&&v>=0))return null;
  return {...e,faceValueLessCostMicroCny:e.uncostedRequests>0?null:e.faceValueLessCostMicroCny,faceValueMarginPercentage:e.uncostedRequests>0?null:e.faceValueMarginPercentage};
}


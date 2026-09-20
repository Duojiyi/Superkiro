export interface Adjustment {operator:string;cardId:string;delta:number;reason:string;key:string}
const storageKey=(operator:string)=>`superkiro.pending-adjustment.v1:${encodeURIComponent(operator)}`;
function validate(value:unknown,operator:string):Adjustment {
  const v=value as Partial<Adjustment>|null;
  if(!operator||operator.length>128||!v||v.operator!==operator||typeof v.cardId!=='string'||!v.cardId||v.cardId.length>256||typeof v.delta!=='number'||!Number.isFinite(v.delta)||v.delta===0||typeof v.reason!=='string'||!v.reason.trim()||v.reason.length>500||typeof v.key!=='string'||! /^[a-zA-Z0-9_.:-]{1,128}$/.test(v.key))throw new Error('保存的调账意图无效，请人工核对账本；未发送新请求');
  return {operator,cardId:v.cardId,delta:v.delta,reason:v.reason,key:v.key};
}
export function loadAdjustment(storage:Storage,operator:string):Adjustment|null {
  const raw=storage.getItem(storageKey(operator));
  if(raw===null)return null;
  try{return validate(JSON.parse(raw),operator);}catch{throw new Error('保存的调账意图损坏，请人工核对账本；不要创建重复调账');}
}
export function saveAdjustment(storage:Storage,intent:Adjustment):void {
  const safe=validate(intent,intent.operator),old=loadAdjustment(storage,intent.operator);
  if(!old&&(Math.abs(safe.delta)>1_000_000||Math.round(safe.delta*1_000_000)/1_000_000!==safe.delta))throw new Error('调账最多六位小数，范围为 ±1,000,000 积分；未发送请求');
  if(old&&JSON.stringify(old)!==JSON.stringify(safe))throw new Error('已有未确认调账，请先使用原意图重试');
  storage.setItem(storageKey(intent.operator),JSON.stringify(safe));
  if(storage.getItem(storageKey(intent.operator))!==JSON.stringify(safe))throw new Error('无法保存重试信息，尚未发送调账');
}
export function clearAdjustment(storage:Storage,intent:Adjustment):void {
  if(loadAdjustment(storage,intent.operator)?.key===intent.key)storage.removeItem(storageKey(intent.operator));
}

// The gateway rounds to whole microcredits; the ledger rejects zero before any write.
// Keep load permissive so legacy zero-micro intents can be inspected and explicitly cleared.
export function isZeroMicroAdjustment(intent:Adjustment):boolean {
  return Number.isFinite(intent.delta)&&intent.delta!==0&&Math.abs(intent.delta*1_000_000)<0.5;
}
export function isUnsubmittedAdjustmentRejection(status:number,message:string,intent:Adjustment):boolean {
  return status===400&&message==='Invalid balance adjustment: Adjustment delta cannot be zero'&&isZeroMicroAdjustment(intent);
}

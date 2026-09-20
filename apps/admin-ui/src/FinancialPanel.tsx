import {useEffect,useRef,useState} from 'react';
import {adminApi,AdminApiError,type AdminFinancials,type CommercialConfig} from './api';
import {estimatedMoney,financialEstimates,parseFinancialSettings} from './financial';

export default function FinancialPanel({data,onPublished,onDirtyChange,onBusyChange}:{data:AdminFinancials|null;onPublished:()=>Promise<void>;onDirtyChange:(dirty:boolean)=>void;onBusyChange:(busy:boolean)=>void}){
  const [config,setConfig]=useState<CommercialConfig|null>(null),[face,setFace]=useState(''),[rate,setRate]=useState(''),[reason,setReason]=useState(''),[message,setMessage]=useState(''),[refreshMessage,setRefreshMessage]=useState(''),[busy,setBusy]=useState(false);
  const pending=useRef(false),alive=useRef(true);
  const [needsReview,setNeedsReview]=useState(false);
  const dirty=!!reason.trim() || (!!config?.settings && (face!==String(config.settings.credit_face_value_cny)||rate!==String(config.settings.usd_cny_rate)));
  let inputError='';
  if(config?.settings){try{parseFinancialSettings(face,rate);}catch(error){inputError=error instanceof Error?error.message:'请填写有效数值';}}
  const reasonBytes=new TextEncoder().encode(reason.trim()).length;
  useEffect(()=>{onBusyChange(busy);return()=>onBusyChange(false);},[busy,onBusyChange]);
  useEffect(()=>{onDirtyChange(dirty);},[dirty,onDirtyChange]);
  const apply=(next:CommercialConfig)=>{setConfig(next);setFace(next.settings?String(next.settings.credit_face_value_cny):'');setRate(next.settings?String(next.settings.usd_cny_rate):'');};
  async function load(){
    if(pending.current)return;pending.current=true;setBusy(true);setRefreshMessage('');setMessage('正在读取财务配置…');
    try{const result=await adminApi.getCommercialConfig();if(result.success!==true||!result.config?.revision)throw new Error('配置读取未确认');if(alive.current){apply(result.config);setNeedsReview(false);setReason('');setRefreshMessage('');setMessage(result.config.settings?'当前财务配置已读取。修改数值并填写原因后发布。':'服务端未提供财务配置，暂不可发布。');}}
    catch(error){if(alive.current){setNeedsReview(true);setMessage(`${error instanceof Error?error.message:'配置读取失败'}。原输入已保留；重新读取成功前不可发布。`);}}finally{pending.current=false;if(alive.current)setBusy(false);}
  }
  useEffect(()=>{alive.current=true;void load();return()=>{alive.current=false;};},[]);
  async function publish(){
    if(pending.current||needsReview||!config?.settings)return;
    let submitted=false;
    try{
      const settings=parseFinancialSettings(face,rate);
      if(!reason.trim()||reasonBytes>500||/[\x00-\x1f\x7f-\x9f]/.test(reason))throw new Error('请填写变更原因，不超过 500 字节且不能包含控制字符（中文通常占 3 字节）');
      if(settings.credit_face_value_cny===config.settings.credit_face_value_cny&&settings.usd_cny_rate===config.settings.usd_cny_rate)throw new Error('数值与当前版本一致，无需重复发布');
      if(!window.confirm(`确认发布财务配置？\n积分面值：${config.settings.credit_face_value_cny} → ${settings.credit_face_value_cny} 元 / 积分\n采购汇率：${config.settings.usd_cny_rate} → ${settings.usd_cny_rate} CNY / USD\n影响成本加成计费及估算口径，不会改变卡密余额，也不是实际收入。有未结算请求时服务端将拒绝。\n原因：${reason.trim()}`))return;
      pending.current=true;submitted=true;setBusy(true);setRefreshMessage('');setMessage('正在发布，请勿重复提交…');
      const result=await adminApi.publishCommercialConfig({settings,expected_revision:config.revision,reason:reason.trim()});
      if(result.success!==true)throw new AdminApiError('服务端未确认发布',400);
      if(!result.config?.settings||!result.config.revision)throw new Error('服务端未返回可核对的配置版本');
      if(alive.current){
        apply(result.config);setReason('');setNeedsReview(false);setMessage('配置已发布，正在刷新账本估算。');
        try{await onPublished();if(alive.current)setRefreshMessage('配置写入已确认；无需重复发布。请留意估算读取提示。');}
        catch{if(alive.current){setRefreshMessage('配置写入已确认；无需重复发布。');setMessage('配置已发布，但账本估算刷新失败。请使用页面“刷新”重新读取估算。');}}
      }
    }catch(error){
      if(alive.current){
        const mustReview=submitted&&!(error instanceof AdminApiError&&[400,401,403,413,422].includes(error.status));
        if(mustReview)setNeedsReview(true);
        setMessage(`${error instanceof Error?error.message:'发布失败'}${mustReview?'。原输入已保留，发布已暂停。请重新读取财务配置核对结果或版本冲突，不要重复提交。':''}`);
      }
    }finally{if(submitted){pending.current=false;if(alive.current)setBusy(false);}}
  }
  const e=financialEstimates(data),margin=e?.faceValueMarginPercentage;
  return <div className="two-columns"><section className="panel"><h3>财务估算配置</h3>
    <p className="muted">积分面值不是实收单价；采购汇率用于 USD 成本换算。采购价格在模型价格版本中配置。修改会影响成本加成计费和估算口径，不会给卡密充值或调整余额；固定积分售价需在“模型与定价”另行调整。</p>
    <form onSubmit={event=>{event.preventDefault();void publish();}}><fieldset disabled={busy||!config?.settings} className="field-grid">
      <label>积分面值（元 / 积分）<input type="number" min="0" step="any" max="1000" required aria-invalid={!!inputError} aria-describedby="financial-value-help" value={face} onChange={event=>setFace(event.target.value)}/></label>
      <label>采购汇率（CNY / USD）<input type="number" min="0" step="any" max="1000" required aria-invalid={!!inputError} aria-describedby="financial-value-help" value={rate} onChange={event=>setRate(event.target.value)}/></label>
      <p id="financial-value-help" className="muted">两项数值均须大于 0、至多 1000。允许小数；留空不表示 0。</p>
      <label>财务配置变更原因<input required maxLength={500} aria-describedby="financial-reason-help" value={reason} onChange={event=>setReason(event.target.value)}/></label>
      <p id="financial-reason-help" className="muted">用于审计，已填写 {reasonBytes} / 500 字节（中文通常占 3 字节）。</p>
      <button type="submit" disabled={needsReview || !!inputError || !reason.trim() || reasonBytes>500} className="primary">确认并发布财务配置</button></fieldset></form>
    <button disabled={busy} onClick={()=>{if(!dirty||window.confirm('重新读取将丢弃未发布的财务编辑，继续吗？'))void load();}}>重新读取财务配置</button>
    {inputError&&<p role="alert">{inputError}；当前输入尚未发布。</p>}
    {needsReview&&<p role="alert">请重新读取财务配置核对，当前禁止发布；读取失败不会解除限制。</p>}
    <p role="status">{message}</p>{refreshMessage&&<p role="status" className="muted">{refreshMessage}</p>}<p className="muted">版本：{config?.revision??'未读取'} · 更新时间：{config?.settings?.rate_updated_at_secs?new Date(config.settings.rate_updated_at_secs*1000).toLocaleString():'未提供'}（服务端记录）</p>
  </section><section className="panel"><h3>保留账本估算</h3><p className="muted">仅覆盖当前保留的 usage 账本，不代表全历史收付款、采购发票或实际毛利。</p>
    {!e?<p role="status">估算契约未提供或尚未读取，暂不计算。</p>:<><p>积分面值估算：{estimatedMoney(e.usageFaceValueMicroCny)}</p><p>已配置采购成本估算：{estimatedMoney(e.configuredProviderCostMicroCny)}{e.uncostedRequests>0?'（部分覆盖）':''}</p><p>已覆盖 {e.costedRequests} 笔 · 未覆盖 {e.uncostedRequests} 笔</p>
      {e.uncostedRequests>0?<p role="status">成本覆盖不完整，差额与比例暂不计算；未定价请求不视为免费。</p>:<><p>积分面值减成本（非实际利润）：{estimatedMoney(e.faceValueLessCostMicroCny)}</p><p>面值差额比例：{typeof margin==='number'&&Number.isFinite(margin)?`${margin.toFixed(2)}%`:'暂不计算'}</p></>}
    </>}
    <p>实际到账收入：未关联收付款账本</p><p>实际毛利：暂不计算</p>
  </section></div>;
}

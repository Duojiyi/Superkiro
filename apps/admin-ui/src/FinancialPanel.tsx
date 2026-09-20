import {useEffect,useRef,useState} from 'react';
import {adminApi,type AdminFinancials,type CommercialConfig} from './api';
import {estimatedMoney,financialEstimates,parseFinancialSettings} from './financial';

export default function FinancialPanel({data,onPublished,onDirtyChange,onBusyChange}:{data:AdminFinancials|null;onPublished:()=>Promise<void>;onDirtyChange:(dirty:boolean)=>void;onBusyChange:(busy:boolean)=>void}){
  const [config,setConfig]=useState<CommercialConfig|null>(null),[face,setFace]=useState(''),[rate,setRate]=useState(''),[reason,setReason]=useState(''),[message,setMessage]=useState(''),[busy,setBusy]=useState(false);
  const pending=useRef(false),alive=useRef(true);
  useEffect(()=>{onBusyChange(busy);return()=>onBusyChange(false);},[busy,onBusyChange]);
  useEffect(()=>{onDirtyChange(!!reason.trim() || (!!config?.settings && (face!==String(config.settings.credit_face_value_cny)||rate!==String(config.settings.usd_cny_rate))));},[config,face,rate,reason,onDirtyChange]);
  const apply=(next:CommercialConfig)=>{setConfig(next);setFace(next.settings?String(next.settings.credit_face_value_cny):'');setRate(next.settings?String(next.settings.usd_cny_rate):'');};
  async function load(){
    if(pending.current)return;pending.current=true;setBusy(true);setMessage('正在读取财务配置…');
    try{const result=await adminApi.getCommercialConfig();if(!result.success)throw new Error('配置读取未确认');if(alive.current){apply(result.config);setReason('');setMessage(result.config.settings?'':'服务端未提供财务配置，暂不可发布。');}}
    catch(error){if(alive.current){setConfig(null);setMessage(error instanceof Error?error.message:'配置读取失败');}}finally{pending.current=false;if(alive.current)setBusy(false);}
  }
  useEffect(()=>{alive.current=true;void load();return()=>{alive.current=false;};},[]);
  async function publish(){
    if(pending.current||!config?.settings)return;
    try{
      const settings=parseFinancialSettings(face,rate);if(!reason.trim())throw new Error('请填写变更原因');
      if(!window.confirm('确认发布积分面值与采购汇率？这会影响成本加成计费及估算口径，并非实际收入。有未结算请求时服务端将拒绝。'))return;
      pending.current=true;setBusy(true);setMessage('正在发布…');
      const result=await adminApi.publishCommercialConfig({settings,expected_revision:config.revision,reason:reason.trim()});
      if(!result.success)throw new Error('服务端未确认发布');
      if(alive.current){apply(result.config);setReason('');setMessage('配置已发布，正在刷新账本估算。');await onPublished();}
    }catch(error){if(alive.current)setMessage(`${error instanceof Error?error.message:'发布失败'}。结果不明确或版本冲突时，请重新读取核对，不要盲目重复发布。`);}
    finally{pending.current=false;if(alive.current)setBusy(false);}
  }
  const e=financialEstimates(data),margin=e?.faceValueMarginPercentage;
  return <div className="two-columns"><section className="panel"><h3>财务估算配置</h3>
    <p className="muted">积分面值不是实收单价；采购汇率用于 USD 成本换算。采购价格在模型价格版本中配置。</p>
    <form onSubmit={event=>{event.preventDefault();void publish();}}><fieldset disabled={busy||!config?.settings} className="field-grid">
      <label>积分面值（元 / 积分）<input type="number" step="any" max="1000" required value={face} onChange={event=>setFace(event.target.value)}/></label>
      <label>采购汇率（CNY / USD）<input type="number" step="any" max="1000" required value={rate} onChange={event=>setRate(event.target.value)}/></label>
      <label>财务配置变更原因<input required maxLength={500} value={reason} onChange={event=>setReason(event.target.value)}/></label>
      <button type="submit" className="primary">确认并发布财务配置</button></fieldset></form>
    <button disabled={busy} onClick={()=>{if(window.confirm('重新读取将丢弃未发布的财务编辑，继续吗？'))void load();}}>重新读取财务配置</button>
    <p role="status">{message}</p><p className="muted">版本：{config?.revision??'未读取'} · 更新时间：{config?.settings?.rate_updated_at_secs?new Date(config.settings.rate_updated_at_secs*1000).toLocaleString():'未提供'}（服务端记录）</p>
  </section><section className="panel"><h3>保留账本估算</h3><p className="muted">仅覆盖当前保留的 usage 账本，不代表全历史收付款、采购发票或实际毛利。</p>
    {!e?<p role="status">估算契约未提供或尚未读取，暂不计算。</p>:<><p>积分面值估算：{estimatedMoney(e.usageFaceValueMicroCny)}</p><p>已配置采购成本估算：{estimatedMoney(e.configuredProviderCostMicroCny)}{e.uncostedRequests>0?'（部分覆盖）':''}</p><p>已覆盖 {e.costedRequests} 笔 · 未覆盖 {e.uncostedRequests} 笔</p>
      {e.uncostedRequests>0?<p role="status">成本覆盖不完整，差额与比例暂不计算；未定价请求不视为免费。</p>:<><p>积分面值减成本（非实际利润）：{estimatedMoney(e.faceValueLessCostMicroCny)}</p><p>面值差额比例：{typeof margin==='number'&&Number.isFinite(margin)?`${margin.toFixed(2)}%`:'暂不计算'}</p></>}
    </>}
    <p>实际到账收入：未关联收付款账本</p><p>实际毛利：暂不计算</p>
  </section></div>;
}

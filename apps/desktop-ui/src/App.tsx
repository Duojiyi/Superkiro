import { MemoryVisual } from './MemoryVisual';
import { Announcements } from './Announcements';
import { listen } from '@tauri-apps/api/event';
import { useEffect, useRef, useState, type ReactNode } from 'react';
import { api, native, number, finite, maintenanceText, recoveryPending, configured, expired, safeError, gateway, sanitizeChecks, levels, report, operationFailureSummary, type Authorization, type Status, type Usage, type Memory, type Check } from './bridge';
type Page = 'login'|'overview'|'doctor'|'settings'|'connecting'|'restore-failed'|'report';
type Intent = 'activate'|'restore'|'switch'|'exit'|'unbind'|'trim';
type Modal = {title: string; detail: string; label: string; action?: Intent};
const tabs = [['overview','概览','▦'],['settings','设置','☷']] as const;
function Empty({title, children}: {title:string; children: ReactNode}) { return <div key={title} className="empty"><div className="empty-bars" aria-hidden="true"><i/><i/><i/></div><h2>{title}</h2><p>{children}</p></div>; }
function Wave({active = false, singlePeak = false}: {active?:boolean; singlePeak?:boolean}) { return <div className={`wave ${active?'responding':''}`} aria-hidden="true">{Array.from({length:singlePeak?33:64},(_,i)=><i key={i} style={{animationDelay:`${-i*.09}s`,height:`${singlePeak?12+84*Math.exp(-(((i-16)/8)**2)):12+60*Math.exp(-(((i-18)/10)**2))+30*Math.exp(-(((i-48)/7)**2))}%`}}/>)}</div>; }
function Confirm({value, close, accept}: {value:Modal; close:()=>void; accept:(card:string)=>void}) {
  const ref = useRef<HTMLDialogElement>(null); const cancel = useRef<HTMLButtonElement>(null); const [card,setCard]=useState('');
  useEffect(()=>{ const opener=document.activeElement as HTMLElement; ref.current?.showModal(); cancel.current?.focus(); return ()=>opener?.focus(); },[]);
  return <dialog ref={ref} onCancel={e=>{e.preventDefault();close();}} aria-labelledby="confirm-title" aria-describedby="confirm-detail"><form onSubmit={e=>{e.preventDefault();accept(card.trim());}}><h2 id="confirm-title">{value.title}</h2><p id="confirm-detail">{value.detail}</p>{value.action==='unbind'&&<label>当前卡密<input type="password" required maxLength={256} autoComplete="off" value={card} onChange={e=>setCard(e.target.value)}/></label>}<button ref={value.action?undefined:cancel} className="primary full" type="submit">{value.label}</button>{value.action&&<button ref={cancel} className="full" type="button" onClick={close}>取消</button>}</form></dialog>;
}
export function App() {
  const [page,setPage]=useState<Page>('login'), [status,setStatus]=useState<Status>({}), [auth,setAuth]=useState<Authorization|null>(null);
  const [card,setCard]=useState(''), [verified,setVerified]=useState<{card:string; gateway:string}|null>(null), [gatewayInput]=useState('');
  const [closeBehavior,setCloseBehavior]=useState('tray');
  async function saveCloseBehavior(value:string){await perform(async()=>{await native('set_close_behavior',[value]);setCloseBehavior(value);});}
  useEffect(()=>{void native<string>('get_close_behavior').then(value=>{if(['tray','minimize','exit'].includes(value))setCloseBehavior(value);}).catch(()=>{});},[]);
  const [busy,setBusy]=useState(false), [uncertain,setUncertain]=useState(false), [notice,setNotice]=useState(''), [loginError,setLoginError]=useState(''), [modal,setModal]=useState<Modal|null>(null);
  const [remember,setRemember]=useState(false), [storeReady,setStoreReady]=useState(false), [elapsed,setElapsed]=useState(0);
  const [usage,setUsage]=useState<Usage|null>(null), [usageState,setUsageState]=useState('idle');
  const [failureSummary,setFailureSummary]=useState('尚未读取最近失败操作。');
  const [checks,setChecks]=useState<Check[]>([]), [doctorState,setDoctorState]=useState('idle'), [doctorTime,setDoctorTime]=useState('');
  const [memoryDetails,setMemoryDetails]=useState<Memory|null>(null);
  const [samples,setSamples]=useState<number[]>([]), [memoryState,setMemoryState]=useState('idle'), [sampleTime,setSampleTime]=useState('');
  const sessionStatus=useRef<Status>({}), verificationOnly=useRef(false), sessionGateway=useRef('');
  const [balanceTime,setBalanceTime]=useState('');
  const [settingsGroup,setSettingsGroup]=useState('install');
  const [hostUnavailable,setHostUnavailable]=useState(false);
  const sessionGeneration=useRef(0), operationBaseline=useRef(0), mutationSent=useRef(false);
  const memoryGeneration=useRef(0);
  const lock=useRef(false), sampling=useRef(false), loadingUsage=useRef(false), mutation=useRef(false);
  const needsRecovery=recoveryPending(status), usageUnavailable=usageState==='error'||usageState==='loading', isConfigured=configured(status), isExpired=expired(auth), ready=isConfigured&&status.model_service_available===true;
  const hasSession=auth!==null||isConfigured;
  const installLabel=status.platform==='darwin'?'选择 Kiro.app':status.platform==='win32'?'选择 Kiro 安装文件夹':'选择 Kiro 安装位置';
  const plan=['PRO','PRO+','PRO Max','Power'].includes(auth?.virtualPlanName||'') ? auth!.virtualPlanName : '套餐待查询';
  const expiry=finite(auth?.validUntil)?`${new Date(auth.validUntil*1000).toLocaleDateString('zh-CN')} 到期` : auth?.status==='unactivated'?'启用后开始计时':'有效期待查询';
  function applyStatus(s:Status){if(s.gateway_url)sessionGateway.current=s.gateway_url;sessionStatus.current=s;setStatus(s);}
  function confirmAuthorization(a:Authorization){setAuth(previous=>({...previous,...a,...(!finite(a.remainingPoints)&&finite(previous?.remainingPoints)?{remainingPoints:previous.remainingPoints}: {})}));if(finite(a.remainingPoints))setBalanceTime(new Date().toLocaleTimeString('zh-CN'));}
  async function refresh() { const generation=sessionGeneration.current;const s=await api<Status>('/api/status');if(generation!==sessionGeneration.current)return sessionStatus.current;applyStatus(s); if(configured(s)&&s.authorization&&!verificationOnly.current)confirmAuthorization(s.authorization); return s; }
  const transientNotice = /^(连接配置已应用|Kiro 配置已还原|已复制|系统保存的卡密已清除)/.test(notice);
  useEffect(()=>{if(!transientNotice)return;const timer=window.setTimeout(()=>setNotice(''),5000);return()=>window.clearTimeout(timer);},[notice,transientNotice]);
  async function perform(work:()=>Promise<void>) { if(lock.current)return; lock.current=true;setBusy(true); try{await work();}catch(e){setNotice(safeError(e));}finally{lock.current=false;setBusy(false);} }
  async function mutate(path:string,body:object={}) {
    mutationSent.current=false;
    if(mutation.current)throw new Error('timeout');
    const before=await api<{id:number}>('/api/operation');if(!Number.isSafeInteger(before.id))throw new Error('invalid operation status');operationBaseline.current=before.id;
    let timer:ReturnType<typeof setTimeout>|undefined;
    mutationSent.current=true;
    try { const result=await Promise.race([api<{success:boolean}>(path,'POST',body),new Promise<never>((_,reject)=>{timer=setTimeout(()=>reject(new Error('timeout')),125000);})]); if(result.success!==true)throw new Error('timeout'); }
    catch(e) { if(/timeout|超时/i.test(String(e))){mutation.current=true;setUncertain(true);} throw e; } finally{clearTimeout(timer);}
  }
  function invalidateMemory(){memoryGeneration.current++;sampling.current=false;setMemoryDetails(null);setSamples([]);setSampleTime('');setMemoryState('idle');}
  function resetSessionData(){sessionGateway.current='';invalidateMemory();sessionGeneration.current++;loadingUsage.current=false;setUsage(null);setUsageState('idle');setAuth(null);setBalanceTime('');verificationOnly.current=false;}
  async function loadUsage() { if(verificationOnly.current||!configured(sessionStatus.current)){setUsageState('disconnected');return;}const generation=sessionGeneration.current;if(loadingUsage.current)return; loadingUsage.current=true;setUsageState('loading');try{
    const data=await api<Usage>('/api/usage');if(generation!==sessionGeneration.current)return;const credit=data.usage?.usageBreakdownList?.find(c=>c.dimensionType==='CREDIT');
    if(!finite(credit?.currentUsageWithPrecision)||!finite(credit?.usageLimitWithPrecision))throw new Error('invalid');
    setUsage(data);setAuth(a=>({...a,totalPoints:credit.usageLimitWithPrecision,...(finite(data.usage?.availableCredits)?{remainingPoints:data.usage.availableCredits}:{}),...(data.usage?.virtualPlanName?{virtualPlanName:data.usage.virtualPlanName}:{}),...(finite(data.usage?.validUntil)?{validUntil:data.usage.validUntil}:{}),...(typeof data.usage?.isExpired==='boolean'?{isExpired:data.usage.isExpired}:{})}));if(finite(data.usage?.availableCredits))setBalanceTime(new Date().toLocaleTimeString('zh-CN'));setUsageState('ready');
  }catch(e){if(generation===sessionGeneration.current){setUsageState('error');if(/Authentication rejected: (401|403)\b/.test(String(e)))setNotice(safeError(e));await refresh().catch(()=>{});}}finally{if(generation===sessionGeneration.current)loadingUsage.current=false;} }
  useEffect(()=>{
    if(!uncertain)return;
    let alive=true, checking=false;
    async function reconcile(){
      if(checking)return;checking=true;
      try{
        const op=await api<{id:number;state:string}>('/api/operation');
        if(!alive||!Number.isSafeInteger(op.id)||op.id<=operationBaseline.current||!['succeeded','failed'].includes(op.state))return;
        const next=await api<Status>('/api/status');if(!alive)return;
        applyStatus(next);if(next.authorization&&!verificationOnly.current)confirmAuthorization(next.authorization);mutation.current=false;setUncertain(false);
        setNotice(op.state==='succeeded'?'宿主操作已结束。请核对当前配置；现在可还原或退出。':'宿主操作已失败并结束。备份仍由宿主管理，现在可诊断并重试还原。');
      }catch{/* Keep mutations blocked while the host result is unavailable. */}finally{checking=false;}
    }
    void reconcile();const timer=setInterval(()=>void reconcile(),2000);
    return()=>{alive=false;clearInterval(timer);};
  },[uncertain]);
  async function sampleMemory(afterTrim=false) { if(sampling.current||(lock.current&&!afterTrim)||mutation.current)return;
    const generation=memoryGeneration.current, session=sessionGeneration.current;
    const current=()=>generation===memoryGeneration.current&&session===sessionGeneration.current;
    sampling.current=true;setMemoryState('loading');try{
    const s=await api<Status>('/api/status');if(!current())return;applyStatus(s);
    if(s.kiro_installed!==true||s.process_state!=='Running'){setMemoryDetails(null);setSamples([]);setMemoryState('empty');return;}
    const data=await api<Memory>('/api/memory/sample');if(!current())return;
    if(!finite(data.total_memory_mb))throw new Error('invalid');
    if(data.total_process_count===0){setMemoryDetails(null);setSamples([]);setMemoryState('empty');return;}
    setMemoryDetails(data);setSamples(v=>[...v,data.total_memory_mb!].slice(-40));setSampleTime(new Date().toLocaleTimeString('zh-CN'));setMemoryState('ready');
  }catch{if(current()){setMemoryDetails(null);setMemoryState('error');}}finally{if(current())sampling.current=false;} }
  async function doctor() { setDoctorState('loading');setChecks([]);try{const data=await api<{items:{name:string;level:string}[]}>('/api/doctor?gateway_url='+encodeURIComponent(gateway(gatewayInput)));if(!Array.isArray(data.items)||!data.items.length)throw new Error('invalid');setChecks(sanitizeChecks(data.items));setDoctorTime(new Date().toLocaleString('zh-CN'));setDoctorState('ready');}catch(e){setDoctorState('error');throw e;} }
  useEffect(()=>{
    if(page!=='doctor')return;
    let alive=true;setFailureSummary('正在读取最近失败操作。');
    void api<unknown>('/api/operation').then(value=>{if(alive)setFailureSummary(operationFailureSummary(value));}).catch(()=>{if(alive)setFailureSummary('最近失败操作读取失败；不影响其他诊断结果，请重新检测。');});
    return()=>{alive=false;};
  },[page,doctorTime]);
  function navigate(next:Page){if(!hasSession&&!['login','doctor','report','restore-failed'].includes(next))return;if(lock.current&&page==='connecting')return;setPage(next);if(next==='settings')void sampleMemory();}
  useEffect(()=>{let alive=true;void refresh().then(s=>{if(alive&&recoveryPending(s)){setPage('doctor');setNotice('检测到待恢复的本机配置，无需卡密即可诊断或还原。');}else if(alive&&configured(s)){setPage('overview');void loadUsage();}}).catch(e=>{if(alive)setNotice(safeError(e));});void native<string|null>('get_remembered_card').then(v=>{if(alive&&(v===null||typeof v==='string'&&v.length<=256)){setStoreReady(true);if(v){setCard(v);setRemember(true);}}}).catch(()=>{});return()=>{alive=false;};},[]);
  useEffect(()=>{if(page==='overview')void sampleMemory();},[page]);
  const windowMode=page==='login'?'connect':'status';
  useEffect(()=>{void native('screen',[windowMode]).catch(()=>{});},[windowMode]);
  useEffect(()=>{if(page!=='connecting')return;setElapsed(0);const start=Date.now();const timer=setInterval(()=>setElapsed(Math.floor((Date.now()-start)/1000)),1000);return()=>clearInterval(timer);},[page]);
  useEffect(()=>{if(!isConfigured&&page!=='settings')return;const timer=setInterval(()=>{if(!document.hidden&&!lock.current){if(page==='overview')void loadUsage();if(page==='settings'||page==='overview')void sampleMemory();}},30000);return()=>clearInterval(timer);},[isConfigured,page]);
  useEffect(()=>{let pending=false,failures=0,disposed=false;const timer=setInterval(()=>{if(pending)return;pending=true;void api('/api/heartbeat','POST').then(()=>{failures=0;if(!disposed)setHostUnavailable(false);}).catch(()=>{if(!disposed&&++failures>=2)setHostUnavailable(true);}).finally(()=>{pending=false;});},15000);return()=>{disposed=true;clearInterval(timer);};},[]);
  async function login(){await perform(async()=>{setLoginError('');setVerified(null);resetSessionData();try{const target=gateway(gatewayInput);const result=await api<{success:boolean;authorization:Authorization;gateway_url?:string}>('/api/verify-card','POST',{gateway_url:target,card_key:card.trim()});if(result.success!==true||!result.authorization)throw new Error('invalid');verificationOnly.current=true;confirmAuthorization(result.authorization);setVerified({card:card.trim(),gateway:result.gateway_url||target});if(remember&&storeReady){try{if(await native('set_remembered_card',[card.trim()])!==true)throw new Error();}catch{setNotice('验证成功，但系统安全存储保存失败。本次卡密仅保留在内存。');}}setCard('');try{await refresh();}catch{setNotice('卡密已验证，本地状态读取失败。请重新检测。');}setPage('overview');}catch(e){setLoginError(safeError(e));}});}
  function ask(action:Intent){if(lock.current)return;if(uncertain){setNotice('上次写入结果未确认，正在查询宿主操作状态，确认结束前暂不允许重复修改配置。');setPage('doctor');return;}
    if(action==='activate'){if(status.recovery_pending||status.has_snapshot){setPage('doctor');setNotice('请先还原 Kiro 配置，再启用连接。');return;}if(!verified){setPage('login');setNotice('请先重新验证卡密，再手动启用连接。');return;}if(isExpired){setPage('overview');return;}if(auth?.remainingPoints===0){setModal({title:'余额不足',detail:'当前积分不足，请充值后重新验证卡密。未修改本机配置。',label:'知道了'});return;}if(status.kiro_installed!==true){setPage('overview');setNotice('请先检测或选择 Kiro 安装位置。');return;}}
    const titles:Record<Intent,string>={activate:'启用连接？',restore:'还原 Kiro 配置？',switch:'切换卡密？',exit:'退出 Superkiro？',unbind:'解除设备绑定？',trim:'整理工作集？'};
    setModal({action,title:titles[action],label:action==='activate'?'启用连接':action==='trim'?'整理工作集':action==='exit'?'还原并退出':'确认并继续',detail:action==='trim'?'仅请求系统整理实际 Kiro 进程工作集，不终止正在编辑的进程。':action==='activate'?'将备份原始配置并配置连接，可能需要重启 Kiro。请先保存文件。配置完成不代表模型对话已验证。':status.has_snapshot===false&&status.recovery_pending===false?(action==='unbind'?'将解除此卡密在当前设备的云端绑定；不会关闭 Kiro 或修改官方配置。':'当前没有待还原的本机配置；不会关闭 Kiro 或修改官方配置。'):'请先保存文件。确认后将自动关闭 Kiro；未能正常退出时会结束进程，未保存内容可能丢失。仅还原 Superkiro 修改的配置，失败保留备份。'});
  }
  useEffect(()=>{let disposed=false;let unlisten:(()=>void)|undefined;void listen('desktop-exit-request',()=>ask('exit')).then(stop=>{if(disposed)stop();else unlisten=stop;}).catch(()=>{});return()=>{disposed=true;unlisten?.();};},[busy,uncertain,page]);
  async function execute(action:Intent,unbindCard:string){setModal(null);await perform(async()=>{
    invalidateMemory();
    if(action==='trim'){if(status.kiro_installed!==true||status.process_state!=='Running')throw new Error('no process');const result=await api<Memory>('/api/memory/trim','POST');await sampleMemory(true);setNotice(finite(result.success_count)&&result.success_count>0?`工作集整理：${result.success_count} 成功，${number(result.failed_count)} 失败。`:'未确认任何 Kiro 进程完成整理，没有可报告的优化结果。');return;}
    if(action==='activate'){setPage('connecting');try{await mutate('/api/activate',{gateway_url:verified!.gateway,card_key:verified!.card,close_kiro_confirmed:true});verificationOnly.current=false;await refresh();setPage('overview');setNotice('连接配置已应用，请在 Kiro 中验证真实模型对话。');void loadUsage();}catch(e){try{await refresh();}catch{if(mutationSent.current){mutation.current=true;setUncertain(true);}}setPage('doctor');setDoctorState('error');throw e;}return;}
    try{if(!(action==='exit'&&status.authenticated===false&&status.has_snapshot===false&&status.recovery_pending!==true))await mutate(action==='unbind'?'/api/unbind':'/api/restore',action==='unbind'?{card_key:unbindCard,gateway_url:verified?.gateway||sessionGateway.current||gateway(gatewayInput),close_kiro_confirmed:true}:{close_kiro_confirmed:true});
      const restoredStatus=await refresh();if(restoredStatus.recovery_pending===true||restoredStatus.has_snapshot===true)throw new Error('restore pending');
      let completionNotice='Kiro 配置已还原。';
      if(action==='switch'||action==='unbind'){try{if(await native('clear_remembered_card')!==true)throw new Error();setRemember(false);}catch{setRemember(true);completionNotice='配置已还原，但系统保存的卡密未能清除，请在登录页取消记住卡密后重试。';}}
      setMemoryDetails(null);setSamples([]);setMemoryState('empty');
      if(action==='restore'&&auth){
        // Restoring IDE configuration does not sign out the desktop session.
        // Invalidate pending usage responses, but retain the last confirmed balance.
        sessionGeneration.current++;loadingUsage.current=false;verificationOnly.current=true;
        if(usageState==='loading')setUsageState(usage?'ready':'idle');
        setPage('overview');setNotice('Kiro 配置已还原，当前卡密与积分信息已保留。');
      }else{
        resetSessionData();setVerified(null);setCard('');setPage('login');
        if(action==='exit')await native('exit');else setNotice(completionNotice);
      }
    }catch(e){setPage('restore-failed');throw e;}
  });}
  async function pick(){await perform(async()=>{const result=await native<{cancelled?:boolean;success?:boolean}|null>('pick_install_path');if(!result||result.cancelled)return;if(result.success!==true)throw new Error('invalid');await refresh();setPage('overview');});}
  const usageZone=usage?.settledUsage?.timezone==='UTC'?'UTC':'时区待确认';
  const reportText=report(checks,doctorTime,status,failureSummary);
  const maintenance=maintenanceText(status.memory_maintenance);
  const memoryText=memoryState==='loading'?'正在采样':memoryState==='error'?'采样失败，当前占用与维护结果未确认':memoryState==='empty'?'Kiro 未运行':memoryState==='ready'?`更新于 ${sampleTime}`:'尚无实际进程采样';
  const pendingLabel=page==='connecting'&&busy?'正在配置连接':page==='doctor'&&doctorState==='loading'?'正在检查':page==='settings'&&memoryState==='loading'?'正在采样':page==='overview'&&usageState==='loading'?'正在读取用量':busy?'正在处理':'';
  return <div className={`shell ${page==='login'?'login-shell':''}`} aria-busy={!!pendingLabel} onMouseDown={event=>{
    if(event.button!==0||(event.target as HTMLElement).closest('button,input,a,select,textarea'))return;
    const header=event.currentTarget.querySelector('header');
    if(header&&event.clientY<=header.getBoundingClientRect().bottom){
      event.preventDefault();
      if(event.detail>1){if(status.platform!=='darwin')void native('maximize').catch(error=>setNotice(safeError(error)));return;}
      void native('drag').catch(error=>setNotice(safeError(error)));
    }
  }}>
    {/* The shell handles header and top padding drag; interactive controls remain clickable. */}
    <header><strong className="brand">Superkiro</strong><div className="window-drag"/>{pendingLabel&&<span className="pending-indicator" role="status"><span className="sr-only">{pendingLabel}</span></span>}<button className="website-button" aria-label="打开官网" title="打开官网" onClick={()=>void perform(async()=>{await native('open_external',['https://kiro.rent/']);})}>官网 ↗</button><Announcements blocked={busy||uncertain||!!modal||!hasSession||page==='login'}/><div className="window-controls"><button aria-label="最小化" onClick={()=>void perform(async()=>{await native('minimize');})}>−</button><button aria-label="关闭窗口" disabled={busy&&!status.tray_available} onClick={()=>closeBehavior==='exit'?ask('exit'):closeBehavior==='minimize'?void native('minimize').catch(e=>setNotice(safeError(e))):status.tray_available?void native('close').catch(e=>setNotice(safeError(e))):ask('exit')}>×</button></div></header>
    {page!=='login'&&hasSession&&<nav aria-label="主导航">{tabs.map(([id,label,icon])=><button key={id} disabled={busy&&page==='connecting'} aria-current={(page===id||id==='overview'&&['connecting','restore-failed'].includes(page))?'page':undefined} onClick={()=>navigate(id)}><span aria-hidden="true">{icon}</span>{label}</button>)}</nav>}
    <div key={page} className="content-scroll" tabIndex={0} role="region" aria-label="页面内容">{(notice||hostUnavailable)&&<aside key={notice} className={`notice ${transientNotice&&!hostUnavailable?'toast timed':''}`}><span role="status" aria-live="polite">{hostUnavailable?'本地服务暂未响应，正在重试检测。':notice}</span><button aria-label="关闭提示" onClick={()=>{setNotice('');setHostUnavailable(false);}}>×</button>{transientNotice&&!hostUnavailable&&<i className="toast-countdown" aria-hidden="true"/>}</aside>}
    <main key={page} className={`page-${page}`}>
    {page==='login'&&<section className="login"><Wave active={busy} singlePeak/><h1>卡密登录</h1><form onSubmit={e=>{e.preventDefault();void login();}}><label className="sr-only" htmlFor="card">输入你的卡密</label><input id="card" type="password" placeholder="输入你的卡密" required maxLength={256} autoComplete="off" spellCheck={false} disabled={busy} value={card} onChange={e=>setCard(e.target.value)} aria-invalid={!!loginError} aria-describedby="login-error"/><p id="login-error" className="warning" role="alert">{loginError}</p><div className="row login-options"><label><input type="checkbox" checked={remember} disabled={!storeReady||busy} onChange={e=>{const checked=e.target.checked;if(checked)setRemember(true);else void perform(async()=>{if(await native('clear_remembered_card')!==true)throw new Error();setRemember(false);setNotice('系统保存的卡密已清除。');});}}/> 记住卡密</label><button className="text" type="button" onClick={()=>setModal({title:'登录帮助',detail:'登录仅验证卡密，不修改本机环境。验证成功后，需手动启用连接。卡密只保留在内存或系统安全存储中。',label:'知道了'})}>登录帮助 ↗</button></div><button className="primary full" disabled={busy}>{busy?'正在验证…':loginError?'重新登录 →':'登录 →'}</button></form></section>}
    {page==='overview'&&(needsRecovery?<section><h1>本机配置待恢复</h1><p>检测到未完成的接入配置，无需卡密即可诊断或还原。</p><button className="primary full" disabled={busy||uncertain} onClick={()=>ask('restore')}>还原 Kiro 配置</button><button className="full" onClick={()=>navigate('doctor')}>查看连接诊断</button></section>:isExpired?<section><h1>卡密已到期</h1><p className="subtitle">当前授权不可用，请更新卡密后继续。</p><div className="panel spaced"><h2>{plan}</h2><p>{expiry}</p><p>剩余积分 {number(auth?.remainingPoints)}（当前不可用）</p></div><button className="primary full" disabled={busy} onClick={()=>ask('switch')}>切换卡密</button><button className="full" disabled={busy} onClick={()=>ask('restore')}>还原 Kiro 配置</button></section>:status.kiro_installed===false?<section><h1>未找到 Kiro</h1><p className="subtitle">先安装 Kiro，再启用连接。</p><Empty title="尚未检测到安装">未修改配置，也未执行内存优化。</Empty><button className="primary full" disabled={busy} onClick={()=>void perform(async()=>{await refresh();})}>重新检测</button><button className="full" disabled={busy||status.has_snapshot} onClick={()=>void pick()}>{installLabel}</button><button className="text full" onClick={()=>void perform(async()=>{await native('open_external',['https://kiro.dev/downloads/']);})}>下载 Kiro ↗</button></section>:<section className="overview-dashboard"><div className="row"><span className="status-badge">{ready?'● 连接正常':isConfigured?'◉ 配置已应用 · 待验证':'◯ 未连接'}</span><button className="text" disabled={busy} onClick={()=>void perform(async()=>{await refresh();await loadUsage();})}>刷新 ↻</button></div><h1>{ready?'Kiro 已就绪':isConfigured?'连接配置已应用':'连接你的 Kiro'}</h1><p className="subtitle">{ready?'模型服务已通过验证':isConfigured?'请在 Kiro 中确认模型列表与真实对话可用':status.kiro_installed?'已检测到 Kiro · 尚未启用':'安装状态尚未确认，请重新检测'}</p><Wave/><div className="balance"><div className="row"><span>剩余积分</span><span className="muted">{plan}</span></div><div className="balance-value">{number(auth?.remainingPoints)}</div>{balanceTime&&<p className="muted">余额更新于 {balanceTime} · 非实时</p>}{usageUnavailable&&<p className="warning" role="status">{usageState==='error'?'用量刷新失败，保留最近确认余额；今日用量暂不可用。':'正在刷新用量，余额仍为最近确认值。'}</p>}<progress aria-label="剩余积分比例" value={finite(auth?.remainingPoints)&&finite(auth?.totalPoints)&&auth.totalPoints>0?Math.min(1,auth.remainingPoints/auth.totalPoints):0} max={1}/><div className="row muted"><span>总积分 {number(auth?.totalPoints)}</span><span>{expiry}</span></div></div><div className="metrics"><div><span title={`按 ${usageZone} 日期统计，仅含已结算请求`}>今日已用积分</span><strong>{number(usageUnavailable?undefined:usage?.settledUsage?.todayPoints)} 积分</strong></div></div><div className="overview-actions"><button className="primary full" disabled={busy||uncertain} onClick={()=>isConfigured?void perform(async()=>{await api('/api/launch','POST');await refresh();setNotice('已发送 Kiro 启动请求。');}):ask('activate')}>{isConfigured?'打开 Kiro ↗':'启用连接'}</button><button className="full" disabled={busy} onClick={()=>isConfigured?ask('restore'):navigate('settings')}>{isConfigured?'还原 Kiro 配置':'检查安装位置'}</button></div><p className="muted">{isConfigured?'配置已应用，请在 Kiro 中验证对话。':'启用后将自动配置并打开 Kiro。'}</p></section>)}
    {page==='connecting'&&<section><Wave active/><h1>正在连接 Kiro</h1><p className="subtitle">正在等待宿主执行并确认结果</p><div className="panel spaced" role="status"><h2>连接配置请求已发送</h2><p>正在配置连接，完成后将自动返回概览。</p><p>已等待 {elapsed} 秒 · 超时阈值 125 秒</p><progress aria-label="正在等待连接结果"/></div><p className="muted">请勿重复提交或退出。超时后保留诊断入口，不会宣称连接成功。</p></section>}
    {page==='doctor'&&<section><div className="row"><h1>连接诊断</h1><button className="text" disabled={busy} onClick={()=>void perform(doctor)}>重新检测 ↻</button></div><div key={doctorState} className={`panel ${doctorState==='error'||checks.some(c=>c.level!=='pass')?'alert':''}`}><h2>{needsRecovery?'本机配置待恢复':doctorState==='loading'?'正在检查':doctorState==='error'?'诊断或连接未完成':doctorState==='ready'?checks.every(c=>c.level==='pass')?'基础检查已通过':'连接需要检查':'尚未执行检查'}</h2><p>{uncertain?'上次修改结果未确认，暂时禁止重复修改，正在自动查询宿主结果，请保留备份。':needsRecovery?'无需卡密即可诊断并还原。请先恢复原始配置，再重新登录启用。':'检查本地安装、授权与连接配置。模型是否可用，请以 Kiro 中的实际对话为准。'}</p></div><p className="muted" aria-live="polite">{failureSummary}</p><ul className="checklist">{checks.map(c=><li key={c.name}><span>{c.name}</span><span className={c.level==='pass'?'positive':'warning'}>{levels[c.level]}</span></li>)}</ul><div className="actions">{needsRecovery&&<button disabled={busy||uncertain} onClick={()=>ask('restore')}>还原待恢复配置</button>}<button className="primary" disabled={doctorState!=='ready'} onClick={()=>setPage('report')}>查看诊断报告</button><button disabled={doctorState!=='ready'} onClick={()=>void perform(async()=>{await navigator.clipboard.writeText(reportText);setNotice('已复制脱敏报告摘要。');})}>复制报告</button></div><button className="text full" onClick={()=>setPage(hasSession?'overview':'login')}>{hasSession?'返回概览':'返回卡密登录'}</button><p className="muted">{doctorTime?`检测于 ${doctorTime}`:'报告只保留脱敏检查摘要。'}</p></section>}
    {page==='restore-failed'&&<section><h1>恢复未完成</h1><p className="subtitle">请先检查恢复结果，再退出程序。</p><div className="panel alert spaced"><h2>无法确认原始配置已恢复</h2><p>请检查文件占用或权限后重试。前端不会删除备份。</p><h2>升级后无法恢复？</h2><ol><li>先保存工作，暂停升级或重装 Kiro。扩展内容改变时会拒绝旧备份覆盖，请勿反复替换扩展或修改哈希。</li><li>保留本机快照、扩展备份和 kiro-byok-desktop-session.json，不删除或编辑。会话与原始备份可能包含凭据，不要公开上传。</li><li>点击下方“查看连接诊断”，选择“重新检测”，再“查看诊断报告”并导出脱敏报告。记录客户端版本、Kiro 升级前后版本和失败时间。</li><li>通过官网支持渠道提交脱敏报告和版本信息，等待确认匹配的恢复方案；未确认前不要清理备份或覆盖安装。本流程尚未完成真实 IDE 升级恢复验收。</li></ol></div><button className="primary full" disabled={busy||uncertain} onClick={()=>ask('restore')}>重新恢复</button><button className="full" onClick={()=>navigate('doctor')}>查看连接诊断</button></section>}
    {page==='report'&&<section><button className="text back" onClick={()=>navigate('doctor')}>← 返回诊断</button><h1>诊断报告</h1><p className="subtitle">卡密、密钥、网关地址与用户目录不会进入报告。</p><pre className="panel">{reportText}</pre><button className="primary full" onClick={()=>{const url=URL.createObjectURL(new Blob([reportText],{type:'text/plain;charset=utf-8'}));const a=document.createElement('a');a.href=url;a.download='Superkiro-diagnostic.txt';a.click();setTimeout(()=>URL.revokeObjectURL(url),1000);setNotice('已发起脱敏报告下载，请确认系统下载结果。');}}>导出脱敏报告</button><button className="full" disabled={busy} onClick={()=>void perform(async()=>{await navigator.clipboard.writeText(reportText);setNotice('已复制脱敏报告摘要。');})}>复制报告摘要</button></section>}
    {page==='settings'&&<section><div className="row"><h1>设置</h1><label className="settings-selector">设置分组 <select value={settingsGroup} onChange={e=>setSettingsGroup(e.target.value)}><option value="install">安装位置</option><option value="memory">内存管理</option><option value="account">账户与窗口</option></select></label></div><div className="settings-group" data-active={settingsGroup==='install'}><h2>Kiro 安装位置</h2><p className={status.kiro_installed?"positive":"muted"}>{status.kiro_installed?'已自动识别'+(status.kiro_version?` · ${status.kiro_version}`:''):'未检测到安装'}</p><div className="path-box" tabIndex={0} title={status.kiro_install_path}>{status.kiro_install_path||(status.kiro_installed?'已检测到安装，完整路径待宿主提供':'尚未选择 Kiro 应用')}</div><div className="actions"><button disabled={busy} onClick={()=>void perform(async()=>{await refresh();})}>重新检测</button><button disabled={busy||status.has_snapshot||status.recovery_pending||uncertain} onClick={()=>void pick()}>{installLabel}</button></div>{status.has_snapshot&&<p className="muted">请先还原 Kiro 配置，再更改安装位置。</p>}</div><div className="divider settings-group memory-card" data-active={settingsGroup==='memory'}><div className="row"><h2>内存管理</h2><span className="muted" title={maintenance.detail}>{maintenance.label}</span></div><MemoryVisual state={memoryState} sample={memoryDetails} samples={samples}/><div className="memory-footer"><p key={memoryState} className="muted memory-status" role="status">{memoryText}</p><div className="memory-controls"><button className="text" disabled={memoryState==='loading'||busy} onClick={()=>void sampleMemory()}>重新采样 ↻</button><button disabled={busy||status.platform!=='win32'||status.kiro_installed!==true||status.process_state!=='Running'||memoryState!=='ready'} onClick={()=>ask('trim')}>立即整理</button></div></div></div><div className="divider settings-group account-card" data-active={settingsGroup==='account'}><div className="row"><h2>账户与窗口</h2><span className="plan-label">{plan}</span></div><div className="row close-preference"><label htmlFor="close-action">关闭窗口时</label><select id="close-action" value={closeBehavior} disabled={busy} onChange={e=>void saveCloseBehavior(e.target.value)}><option value="tray">收至托盘</option><option value="minimize">{status.platform==='darwin'?'最小化到程序坞':'最小化到任务栏'}</option><option value="exit">退出程序</option></select></div><div className="settings-actions"><button disabled={busy} onClick={()=>ask('switch')}>切换卡密</button><button onClick={()=>setModal({title:'Superkiro',detail:`版本 ${status.app_version||'待查询'}`,label:'知道了'})}>版本信息</button><button disabled={busy} onClick={()=>void perform(async()=>{await native('open_external',['https://kiro.rent/#downloads']);})}>下载新版 ↗</button><p className="muted">升级前请保存工作，还原 Kiro 配置并确认成功，再退出客户端安装新版；重新打开后核对版本并验证连接。还原失败时请保留备份，先查看诊断，不要直接覆盖升级。本客户端不自动检查或安装更新。</p><button disabled={busy||!status.tray_available} onClick={()=>void native('close').catch(e=>setNotice(safeError(e)))}>收至托盘</button><button disabled={busy} onClick={()=>ask('exit')}>退出程序</button><button className="danger-action" disabled={busy} onClick={()=>ask('unbind')}>解除设备绑定</button></div></div></section>}
    </main></div>{modal&&<Confirm value={modal} close={()=>setModal(null)} accept={c=>modal.action?void execute(modal.action,c):setModal(null)}/>}
  </div>;
}








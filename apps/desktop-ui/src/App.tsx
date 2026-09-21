import { MemoryVisual } from './MemoryVisual';
import { Announcements } from './Announcements';
import { listen } from '@tauri-apps/api/event';
import { useEffect, useRef, useState, type ReactNode } from 'react';
import { api, native, number, finite, maintenanceText, recoveryPending, configured, expired, gateway, type Authorization, type Status, type Usage, type Memory } from './bridge';
import { toClientError, feedbackText, type ClientError } from './errors';
type Page = 'login'|'overview'|'settings'|'connecting'|'restore-failed';
type Intent = 'activate'|'restore'|'switch'|'exit'|'unbind'|'trim';
type Modal = {title: string; detail: string; label: string; action?: Intent};
const MINIMUM_KIRO_VERSION = '1.1.14';
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
  const [busy,setBusy]=useState(false), [uncertain,setUncertain]=useState(false), [remoteUncertain,setRemoteUncertain]=useState(false), [remoteUnbound,setRemoteUnbound]=useState(false), [notice,setNotice]=useState(''), [loginError,setLoginError]=useState(''), [modal,setModal]=useState<Modal|null>(null);
  const [remember,setRemember]=useState(false), [storeReady,setStoreReady]=useState(false), [elapsed,setElapsed]=useState(0);
  const [usage,setUsage]=useState<Usage|null>(null), [usageState,setUsageState]=useState('idle');
  const [clientError,setClientError]=useState<ClientError|null>(null), [copyNotice,setCopyNotice]=useState('');
  function captureError(error:unknown,showNotice=true){const value=toClientError(error);if(value.code==='SK-BIND-004'&&value.outcome==='partial')setRemoteUnbound(true);setClientError(value);setCopyNotice('');const message=value.message;if(showNotice)setNotice(message);return message;}
  async function copyFeedback(){if(!clientError)return;try{await navigator.clipboard.writeText(feedbackText(clientError,status));setCopyNotice('已复制反馈信息。');}catch{setCopyNotice('复制失败，请记录错误码和反馈编号，通过官网联系支持。');}}
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
  async function perform(work:()=>Promise<void>) { if(lock.current)return; lock.current=true;setBusy(true);setClientError(null);setCopyNotice(''); try{await work();}catch(e){captureError(e);}finally{lock.current=false;setBusy(false);} }
  async function mutate(path:string,body:object={}) {
    mutationSent.current=false;
    if(remoteUnbound&&path==='/api/unbind')throw toClientError({code:'SK-BIND-004',outcome:'partial'});
    const localRecovery=remoteUncertain&&path==='/api/restore';
    if(mutation.current&&!localRecovery)throw toClientError({code:'SK-LOCAL-002',outcome:'unknown'});
    const before=await api<{id:number;state?:string}>('/api/operation');if(!Number.isSafeInteger(before.id))throw new Error('invalid operation status');operationBaseline.current=before.id;
    if(localRecovery){mutation.current=false;setUncertain(false);setRemoteUncertain(false);}
    // A running host operation already owns this ID; no new mutation is sent.
    if(before.state==='running'){operationBaseline.current=before.id-1;mutation.current=true;setUncertain(true);throw toClientError({code:'SK-LOCAL-002',outcome:'unknown'});}
    let timer:ReturnType<typeof setTimeout>|undefined;
    mutationSent.current=true;
    try { const result=await Promise.race([api<{success:boolean}>(path,'POST',body),new Promise<never>((_,reject)=>{timer=setTimeout(()=>reject(toClientError({code:'SK-NET-001',outcome:'unknown'})),125000);})]); if(result?.success!==true)throw toClientError({code:'SK-UNKNOWN-001',outcome:'unknown'}); }
    catch(e) { const error=toClientError(e);
      if(error.code==='SK-LOCAL-002'){
        mutationSent.current=false;
        let tracked=true;
        try{
          const current=await api<{id:number;state:string}>('/api/operation');
          if(Number.isSafeInteger(current.id)){
            if(current.state==='running')operationBaseline.current=current.id-1;
            else if(['idle','succeeded','failed'].includes(current.state))tracked=current.id>before.id;
          }
        }catch{/* Keep writes blocked if the competing operation cannot be checked. */}
        if(tracked){mutation.current=true;setUncertain(true);}
      }else if(error.outcome==='unknown'){mutation.current=true;setUncertain(true);} throw error; } finally{clearTimeout(timer);}
  }
  function invalidateMemory(){memoryGeneration.current++;sampling.current=false;setMemoryDetails(null);setSamples([]);setSampleTime('');setMemoryState('idle');}
  function resetSessionData(){sessionGateway.current='';invalidateMemory();sessionGeneration.current++;loadingUsage.current=false;setUsage(null);setUsageState('idle');setAuth(null);setBalanceTime('');verificationOnly.current=false;}
  async function loadUsage() { if(verificationOnly.current||!configured(sessionStatus.current)){setUsageState('disconnected');return;}const generation=sessionGeneration.current;if(loadingUsage.current)return; loadingUsage.current=true;setUsageState('loading');try{
    const data=await api<Usage>('/api/usage');if(generation!==sessionGeneration.current)return;const credit=data.usage?.usageBreakdownList?.find(c=>c.dimensionType==='CREDIT');
    if(!finite(credit?.currentUsageWithPrecision)||!finite(credit?.usageLimitWithPrecision))throw new Error('invalid');
    setUsage(data);setAuth(a=>({...a,totalPoints:credit.usageLimitWithPrecision,...(finite(data.usage?.availableCredits)?{remainingPoints:data.usage.availableCredits}:{}),...(data.usage?.virtualPlanName?{virtualPlanName:data.usage.virtualPlanName}:{}),...(finite(data.usage?.validUntil)?{validUntil:data.usage.validUntil}:{}),...(typeof data.usage?.isExpired==='boolean'?{isExpired:data.usage.isExpired}:{})}));if(finite(data.usage?.availableCredits))setBalanceTime(new Date().toLocaleTimeString('zh-CN'));setUsageState('ready');
  }catch(e){if(generation===sessionGeneration.current){setUsageState('error');const error=toClientError(e);if(['SK-AUTH-003','SK-AUTH-001'].includes(error.code))captureError(error);await refresh().catch(()=>{});}}finally{if(generation===sessionGeneration.current)loadingUsage.current=false;} }
  useEffect(()=>{
    if(!uncertain||remoteUncertain)return;
    let alive=true, checking=false;
    async function reconcile(){
      if(checking)return;checking=true;
      try{
        const op=await api<{id:number;state:string;support_error?:unknown}>('/api/operation');
        if(!alive||!Number.isSafeInteger(op.id)||op.id<=operationBaseline.current||!['succeeded','failed'].includes(op.state))return;
        const next=await api<Status>('/api/status');if(!alive)return;
        applyStatus(next);if(next.authorization&&!verificationOnly.current)confirmAuthorization(next.authorization);
        if(op.state==='failed'){
          const error=toClientError(op.support_error??{code:'SK-UNKNOWN-001',outcome:'failed'});
          captureError(error);
          if(error.outcome==='unknown'){setRemoteUncertain(true);setNotice('宿主操作已结束，但云端结果未确认。请保留备份并联系支持，不要重复解绑或激活；如需恢复，仅还原本机配置。');return;}
        }else{setClientError(null);setCopyNotice('');setNotice('宿主操作已结束。请核对当前配置；现在可还原或退出。');}
        mutation.current=false;setUncertain(false);
      }catch{/* Keep mutations blocked while the host result is unavailable. */}finally{checking=false;}
    }
    void reconcile();const timer=setInterval(()=>void reconcile(),2000);
    return()=>{alive=false;clearInterval(timer);};
  },[uncertain,remoteUncertain]);
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
  }catch(e){if(current()){setMemoryDetails(null);setMemoryState('error');captureError(e);}}finally{if(current())sampling.current=false;} }
  function navigate(next:Page){if(!hasSession&&!(needsRecovery&&next==='overview')&&!['login','restore-failed'].includes(next))return;if(lock.current&&page==='connecting')return;setPage(next);if(next==='settings')void sampleMemory();}
  useEffect(()=>{let alive=true;void refresh().then(s=>{if(alive&&recoveryPending(s)){setPage('overview');setNotice('检测到待恢复的本机配置，无需卡密即可还原。');}else if(alive&&configured(s)){setPage('overview');void loadUsage();}}).catch(e=>{if(alive)captureError(e);});void native<string|null>('get_remembered_card').then(v=>{if(alive&&(v===null||typeof v==='string'&&v.length<=256)){setStoreReady(true);if(v){setCard(v);setRemember(true);}}}).catch(()=>{});return()=>{alive=false;};},[]);
  useEffect(()=>{if(page==='overview'&&hasSession)void sampleMemory();},[page]);
  const windowMode=page==='login'?'connect':'status';
  useEffect(()=>{void native('screen',[windowMode]).catch(()=>{});},[windowMode]);
  useEffect(()=>{if(page!=='connecting')return;setElapsed(0);const start=Date.now();const timer=setInterval(()=>setElapsed(Math.floor((Date.now()-start)/1000)),1000);return()=>clearInterval(timer);},[page]);
  useEffect(()=>{if(!isConfigured&&page!=='settings')return;const timer=setInterval(()=>{if(!document.hidden&&!lock.current){if(page==='overview')void loadUsage();if(page==='settings'||page==='overview')void sampleMemory();}},30000);return()=>clearInterval(timer);},[isConfigured,page]);
  useEffect(()=>{let pending=false,failures=0,disposed=false;const timer=setInterval(()=>{if(pending)return;pending=true;void api('/api/heartbeat','POST').then(()=>{failures=0;if(!disposed)setHostUnavailable(false);}).catch(()=>{if(!disposed&&++failures>=2)setHostUnavailable(true);}).finally(()=>{pending=false;});},15000);return()=>{disposed=true;clearInterval(timer);};},[]);
  async function login(){await perform(async()=>{setLoginError('');setVerified(null);resetSessionData();try{const target=gateway(gatewayInput);const result=await api<{success:boolean;authorization:Authorization;gateway_url?:string}>('/api/verify-card','POST',{gateway_url:target,card_key:card.trim()});if(result.success!==true||!result.authorization)throw new Error('invalid');verificationOnly.current=true;confirmAuthorization(result.authorization);setVerified({card:card.trim(),gateway:result.gateway_url||target});if(remember&&storeReady){try{if(await native('set_remembered_card',[card.trim()])!==true)throw new Error();}catch(e){captureError(e,false);setNotice('验证成功，但系统安全存储保存失败。本次卡密仅保留在内存。');}}setCard('');try{await refresh();}catch{setNotice('卡密已验证，本地状态读取失败。请重新检测。');}setPage('overview');}catch(e){setLoginError(captureError(e,false));}});}
  function ask(action:Intent){if(lock.current)return;if(remoteUnbound&&['activate','unbind'].includes(action)){setNotice('云端解绑已完成，请仅还原本机配置；恢复完成后重新登录。');return;}if(uncertain&&!(remoteUncertain&&action==='restore')){setNotice(remoteUncertain?'宿主操作已结束，但云端结果未确认。请保留备份并联系支持，不要重复解绑或激活；如需恢复，仅还原本机配置。':'上次写入结果未确认，正在查询宿主操作状态，确认结束前暂不允许重复修改配置。');return;}
    if(action==='activate'){if(status.recovery_pending||status.has_snapshot){setPage('overview');setNotice('请先还原 Kiro 配置，再启用连接。');return;}if(status.kiro_compatible===false){setModal({title:'需要升级 Kiro',detail:`当前 Kiro 版本不受支持。请先升级到 ${status.minimum_kiro_version||MINIMUM_KIRO_VERSION} 或更高版本，再重新检测。未发送启用请求。`,label:'知道了'});return;}if(status.kiro_compatible!==true){setModal({title:'无法确认 Kiro 版本',detail:`暂时无法确认当前版本是否满足最低要求 ${status.minimum_kiro_version||MINIMUM_KIRO_VERSION}。请重新检测，确认兼容后再启用。未发送启用请求。`,label:'知道了'});return;}if(!verified){setPage('login');setNotice('请先重新验证卡密，再手动启用连接。');return;}if(isExpired){setPage('overview');return;}if(auth?.remainingPoints===0){setModal({title:'余额不足',detail:'当前积分不足，请充值后重新验证卡密。未修改本机配置。',label:'知道了'});return;}if(status.kiro_installed!==true){setPage('overview');setNotice('请先检测或选择 Kiro 安装位置。');return;}}
    const titles:Record<Intent,string>={activate:'启用连接？',restore:'还原 Kiro 配置？',switch:'切换卡密？',exit:'退出 Superkiro？',unbind:'解除设备绑定？',trim:'整理工作集？'};
    setModal({action,title:titles[action],label:action==='activate'?'启用连接':action==='trim'?'整理工作集':action==='exit'?'还原并退出':'确认并继续',detail:action==='trim'?'仅请求系统整理实际 Kiro 进程工作集，不终止正在编辑的进程。':action==='activate'?'将备份原始配置并配置连接，可能需要重启 Kiro。请先保存文件。配置完成不代表模型对话已验证。':status.has_snapshot===false&&status.recovery_pending===false?(action==='unbind'?'将解除此卡密在当前设备的云端绑定；不会关闭 Kiro 或修改官方配置。':'当前没有待还原的本机配置；不会关闭 Kiro 或修改官方配置。'):'请先保存文件。确认后将自动关闭 Kiro；未能正常退出时会结束进程，未保存内容可能丢失。仅还原 Superkiro 修改的配置，失败保留备份。'});
  }
  useEffect(()=>{let disposed=false;let unlisten:(()=>void)|undefined;void listen('desktop-exit-request',()=>ask('exit')).then(stop=>{if(disposed)stop();else unlisten=stop;}).catch(()=>{});return()=>{disposed=true;unlisten?.();};},[busy,uncertain,remoteUncertain,remoteUnbound,page]);
  async function execute(action:Intent,unbindCard:string){setModal(null);if(remoteUnbound&&['activate','unbind'].includes(action)){ask(action);return;}if(uncertain&&!(remoteUncertain&&action==='restore')){ask(action);return;}await perform(async()=>{
    invalidateMemory();
    if(action==='trim'){if(status.kiro_installed!==true||status.process_state!=='Running')throw new Error('no process');const result=await api<Memory>('/api/memory/trim','POST');await sampleMemory(true);setNotice(finite(result.success_count)&&result.success_count>0?`工作集整理：${result.success_count} 成功，${number(result.failed_count)} 失败。`:'未确认任何 Kiro 进程完成整理，没有可报告的优化结果。');return;}
    if(action==='activate'){setPage('connecting');try{await mutate('/api/activate',{gateway_url:verified!.gateway,card_key:verified!.card,close_kiro_confirmed:true});verificationOnly.current=false;await refresh();setPage('overview');setNotice('连接配置已应用，请在 Kiro 中验证真实模型对话。');void loadUsage();}catch(e){try{await refresh();}catch{if(mutationSent.current){mutation.current=true;setUncertain(true);}}setPage('overview');captureError(e);return;}return;}
    try{if(!(action==='exit'&&status.authenticated===false&&status.has_snapshot===false&&status.recovery_pending!==true))await mutate(action==='unbind'?'/api/unbind':'/api/restore',action==='unbind'?{card_key:unbindCard,gateway_url:verified?.gateway||sessionGateway.current||gateway(gatewayInput),close_kiro_confirmed:true}:{close_kiro_confirmed:true});
      const restoredStatus=await refresh();if(restoredStatus.recovery_pending===true||restoredStatus.has_snapshot===true)throw new Error('restore pending');
      let completionNotice=action==='unbind'?'设备绑定已解除，Kiro 配置已还原。':'Kiro 配置已还原。';
      if(action==='switch'||action==='unbind'){try{if(await native('clear_remembered_card')!==true)throw new Error();setRemember(false);}catch(e){captureError(e,false);setRemember(true);completionNotice='配置已还原，但系统保存的卡密未能清除，请在登录页取消记住卡密后重试。';}}
      setMemoryDetails(null);setSamples([]);setMemoryState('empty');
      if(action==='restore'&&auth&&!remoteUnbound){
        // The host logs out on restore; retain only the last confirmed authorization.
        // Disconnected usage cannot remain labeled as today's usage.
        sessionGeneration.current++;loadingUsage.current=false;verificationOnly.current=true;
        setUsage(null);setUsageState('disconnected');
        setPage('overview');setNotice('Kiro 配置已还原，当前卡密与积分信息已保留。');
      }else{
        resetSessionData();setRemoteUnbound(false);setVerified(null);setCard('');setPage('login');
        if(action==='exit')await native('exit');else setNotice(completionNotice);
      }
    }catch(e){if(remoteUncertain&&action==='restore'&&!mutation.current){mutation.current=true;setUncertain(true);setRemoteUncertain(true);}if(!mutation.current)setPage('restore-failed');throw e;}
  });}
  async function pick(){await perform(async()=>{const result=await native<{cancelled?:boolean;success?:boolean}|null>('pick_install_path');if(!result||result.cancelled)return;if(result.success!==true)throw new Error('invalid');await refresh();setPage('overview');});}
  const usageZone=usage?.settledUsage?.timezone==='UTC'?'UTC':'时区待确认';
  const maintenance=maintenanceText(status.memory_maintenance);
  const memoryText=memoryState==='loading'?'正在采样':memoryState==='error'?'采样失败，当前占用与维护结果未确认':memoryState==='empty'?'Kiro 未运行':memoryState==='ready'?`更新于 ${sampleTime}`:'尚无实际进程采样';
  const pendingLabel=page==='connecting'&&busy?'正在配置连接':page==='settings'&&memoryState==='loading'?'正在采样':page==='overview'&&usageState==='loading'?'正在读取用量':busy?'正在处理':'';
  return <div className={`shell ${page==='login'?'login-shell':''}`} aria-busy={!!pendingLabel} onMouseDown={event=>{
    if(event.button!==0||(event.target as HTMLElement).closest('button,input,a,select,textarea'))return;
    const header=event.currentTarget.querySelector('header');
    if(header&&event.clientY<=header.getBoundingClientRect().bottom){
      event.preventDefault();
      if(event.detail>1){if(status.platform!=='darwin')void native('maximize').catch(error=>captureError(error));return;}
      void native('drag').catch(error=>captureError(error));
    }
  }}>
    {/* The shell handles header and top padding drag; interactive controls remain clickable. */}
    <header><strong className="brand">Superkiro</strong><div className="window-drag"/>{pendingLabel&&<span className="pending-indicator" role="status"><span className="sr-only">{pendingLabel}</span></span>}<button className="website-button" aria-label="打开官网" title="打开官网" onClick={()=>void perform(async()=>{await native('open_external',['https://kiro.rent/']);})}>官网 ↗</button><Announcements blocked={busy||uncertain||!!modal||!hasSession||page==='login'}/><div className="window-controls"><button aria-label="最小化" onClick={()=>void perform(async()=>{await native('minimize');})}>−</button><button aria-label="关闭窗口" disabled={busy&&!status.tray_available} onClick={()=>closeBehavior==='exit'?ask('exit'):closeBehavior==='minimize'?void native('minimize').catch(e=>captureError(e)):status.tray_available?void native('close').catch(e=>captureError(e)):ask('exit')}>×</button></div></header>
    {page!=='login'&&hasSession&&<nav aria-label="主导航">{tabs.map(([id,label,icon])=><button key={id} disabled={busy&&page==='connecting'} aria-current={(page===id||id==='overview'&&['connecting','restore-failed'].includes(page))?'page':undefined} onClick={()=>navigate(id)}><span aria-hidden="true">{icon}</span>{label}</button>)}</nav>}
    <div key={page} className="content-scroll" tabIndex={0} role="region" aria-label="页面内容">{(notice||hostUnavailable)&&<aside key={notice} className={`notice ${transientNotice&&!hostUnavailable?'toast timed':''}`}><span role="status" aria-live="polite">{hostUnavailable?'本地服务暂未响应，正在重试检测。':notice}</span><button aria-label="关闭提示" onClick={()=>{setNotice('');setHostUnavailable(false);}}>×</button>{transientNotice&&!hostUnavailable&&<i className="toast-countdown" aria-hidden="true"/>}</aside>}
    <main key={page} className={`page-${page}`}>
    {(remoteUncertain||remoteUnbound)&&<aside className="panel alert spaced"><p>{remoteUnbound?'云端解绑已完成。请仅还原本机配置，恢复完成后重新登录。':'云端结果未确认。仅可还原本机配置，不会重试云端解绑或激活。'}</p><button disabled={busy||(uncertain&&!remoteUncertain)} onClick={()=>ask('restore')}>仅还原本机配置</button></aside>}
    {clientError&&<section className="panel alert spaced error-feedback" aria-label="错误反馈"><div className="row"><h2>{clientError.outcome==='partial'?'云端操作已完成，本机仍需处理':clientError.outcome==='unknown'?'操作结果待确认':'操作未完成'}</h2><button aria-label="关闭错误反馈" onClick={()=>{setClientError(null);setCopyNotice('');}}>×</button></div><p>{clientError.message}</p><p>错误码：{clientError.code}</p><p>反馈编号：{clientError.feedback_id}</p><button onClick={()=>void copyFeedback()}>复制反馈信息</button>{copyNotice&&<p role="status">{copyNotice}</p>}</section>}
    {page==='login'&&<section className="login"><Wave active={busy} singlePeak/><h1>卡密登录</h1><form onSubmit={e=>{e.preventDefault();void login();}}><label className="sr-only" htmlFor="card">输入你的卡密</label><input id="card" type="password" placeholder="输入你的卡密" required maxLength={256} autoComplete="off" spellCheck={false} disabled={busy} value={card} onChange={e=>setCard(e.target.value)} aria-invalid={!!loginError} aria-describedby="login-error"/><p id="login-error" className="warning" role="alert">{loginError}</p><div className="row login-options"><label><input type="checkbox" checked={remember} disabled={!storeReady||busy} onChange={e=>{const checked=e.target.checked;if(checked)setRemember(true);else void perform(async()=>{if(await native('clear_remembered_card')!==true)throw new Error();setRemember(false);setNotice('系统保存的卡密已清除。');});}}/> 记住卡密</label><button className="text" type="button" onClick={()=>setModal({title:'登录帮助',detail:'登录仅验证卡密，不修改本机环境。验证成功后，需手动启用连接。卡密只保留在内存或系统安全存储中。',label:'知道了'})}>登录帮助 ↗</button></div><button className="primary full" disabled={busy}>{busy?'正在验证…':loginError?'重新登录 →':'登录 →'}</button></form></section>}
    {page==='overview'&&(needsRecovery?<section><h1>本机配置待恢复</h1><p>检测到未完成的接入配置，无需卡密即可还原。保留备份；仍无法恢复时，请通过官网联系支持。</p><button className="primary full" disabled={busy||uncertain} onClick={()=>ask('restore')}>还原 Kiro 配置</button>{!hasSession&&<button className="text full" onClick={()=>navigate('login')}>返回卡密登录</button>}</section>:isExpired?<section><h1>卡密已到期</h1><p className="subtitle">当前授权不可用，请更新卡密后继续。</p><div className="panel spaced"><h2>{plan}</h2><p>{expiry}</p><p>剩余积分 {number(auth?.remainingPoints)}（当前不可用）</p></div><button className="primary full" disabled={busy} onClick={()=>ask('switch')}>切换卡密</button><button className="full" disabled={busy} onClick={()=>ask('restore')}>还原 Kiro 配置</button></section>:status.kiro_installed===false?<section><h1>未找到 Kiro</h1><p className="subtitle">先安装 Kiro，再启用连接。</p><Empty title="尚未检测到安装">未修改配置，也未执行内存优化。</Empty><button className="primary full" disabled={busy} onClick={()=>void perform(async()=>{await refresh();})}>重新检测</button><button className="full" disabled={busy||status.has_snapshot} onClick={()=>void pick()}>{installLabel}</button><button className="text full" onClick={()=>void perform(async()=>{await native('open_external',['https://kiro.dev/downloads/']);})}>下载 Kiro ↗</button></section>:<section className="overview-dashboard"><div className="row"><span className="status-badge">{ready?'● 连接正常':isConfigured?'◉ 配置已应用 · 待验证':'◯ 未连接'}</span><button className="text" disabled={busy} onClick={()=>void perform(async()=>{await refresh();await loadUsage();})}>刷新 ↻</button></div><h1>{ready?'Kiro 已就绪':isConfigured?'连接配置已应用':'连接你的 Kiro'}</h1><p className="subtitle">{ready?'模型服务已通过验证':isConfigured?'请在 Kiro 中确认模型列表与真实对话可用':status.kiro_installed?'已检测到 Kiro · 尚未启用':'安装状态尚未确认，请重新检测'}</p><Wave/><div className="balance"><div className="row"><span>剩余积分</span><span className="muted">{plan}</span></div><div className="balance-value">{number(auth?.remainingPoints)}</div>{balanceTime&&<p className="muted" title="可用余额已扣除在途预留，预留不计入已用积分">余额更新于 {balanceTime} · 非实时</p>}{usageUnavailable&&<p className="warning" role="status">{usageState==='error'?'用量刷新失败，保留最近确认余额；今日用量暂不可用。':'正在刷新用量，余额仍为最近确认值。'}</p>}<progress aria-label="剩余积分比例" value={finite(auth?.remainingPoints)&&finite(auth?.totalPoints)&&auth.totalPoints>0?Math.min(1,auth.remainingPoints/auth.totalPoints):0} max={1}/><div className="row muted"><span>总积分 {number(auth?.totalPoints)}</span><span>{expiry}</span></div></div><div className="metrics"><div><span title={`按 ${usageZone} 日期统计，仅含已结算请求`}>今日已用积分</span><strong>{number(usageUnavailable?undefined:usage?.settledUsage?.todayPoints)} 积分</strong></div></div><div className="overview-actions"><button className="primary full" disabled={busy||uncertain} onClick={()=>isConfigured?void perform(async()=>{await mutate('/api/launch');await refresh();setNotice('已发送 Kiro 启动请求。');}):ask('activate')}>{isConfigured?'打开 Kiro ↗':'启用连接'}</button><button className="full" disabled={busy} onClick={()=>isConfigured?ask('restore'):navigate('settings')}>{isConfigured?'还原 Kiro 配置':'检查安装位置'}</button></div><p className="muted">{isConfigured?'配置已应用，请在 Kiro 中验证对话。':'启用后将自动配置并打开 Kiro。'}</p></section>)}
    {page==='connecting'&&<section><Wave active/><h1>正在连接 Kiro</h1><p className="subtitle">正在等待宿主执行并确认结果</p><div className="panel spaced" role="status"><h2>连接配置请求已发送</h2><p>正在配置连接，完成后将自动返回概览。</p><p>已等待 {elapsed} 秒 · 超时阈值 125 秒</p><progress aria-label="正在等待连接结果"/></div><p className="muted">请勿重复提交或退出。超时后自动查询宿主结果，不会宣称连接成功。</p></section>}
    {page==='restore-failed'&&<section><h1>恢复未完成</h1><p className="subtitle">请检查文件占用或权限后重试，确认恢复前不要退出。</p><div className="panel alert spaced"><p>保留本机快照和扩展备份，不要删除或编辑，不要公开上传，暂停升级或重装 Kiro。</p><p>仍无法恢复时，请复制反馈信息，通过官网支持渠道提交错误码、反馈编号、客户端与 Kiro 版本及失败时间。</p></div><button className="primary full" disabled={busy||uncertain} onClick={()=>ask('restore')}>重新恢复</button></section>}
    {page==='settings'&&<section><div className="row"><h1>设置</h1><label className="settings-selector">设置分组 <select value={settingsGroup} onChange={e=>setSettingsGroup(e.target.value)}><option value="install">安装位置</option><option value="memory">内存管理</option><option value="account">账户与窗口</option></select></label></div><div className="settings-group" data-active={settingsGroup==='install'}><h2>Kiro 安装位置</h2><p className={status.kiro_installed?"positive":"muted"}>{status.kiro_installed?'已自动识别'+(status.kiro_version?` · ${status.kiro_version}`:''):'未检测到安装'}</p><div className="path-box" tabIndex={0} title={status.kiro_install_path}>{status.kiro_install_path||(status.kiro_installed?'已检测到安装，完整路径待宿主提供':'尚未选择 Kiro 应用')}</div><div className="actions"><button disabled={busy} onClick={()=>void perform(async()=>{await refresh();})}>重新检测</button><button disabled={busy||status.has_snapshot||status.recovery_pending||uncertain} onClick={()=>void pick()}>{installLabel}</button></div>{status.has_snapshot&&<p className="muted">请先还原 Kiro 配置，再更改安装位置。</p>}</div><div className="divider settings-group memory-card" data-active={settingsGroup==='memory'}><div className="row"><h2>内存管理</h2><span className="muted" title={maintenance.detail}>{maintenance.label}</span></div><MemoryVisual state={memoryState} sample={memoryDetails} samples={samples}/><div className="memory-footer"><p key={memoryState} className="muted memory-status" role="status">{memoryText}</p><div className="memory-controls"><button className="text" disabled={memoryState==='loading'||busy} onClick={()=>void sampleMemory()}>重新采样 ↻</button><button disabled={busy||status.platform!=='win32'||status.kiro_installed!==true||status.process_state!=='Running'||memoryState!=='ready'} onClick={()=>ask('trim')}>立即整理</button></div></div></div><div className="divider settings-group account-card" data-active={settingsGroup==='account'}><div className="row"><h2>账户与窗口</h2><span className="plan-label">{plan}</span></div><div className="row close-preference"><label htmlFor="close-action">关闭窗口时</label><select id="close-action" value={closeBehavior} disabled={busy} onChange={e=>void saveCloseBehavior(e.target.value)}><option value="tray">收至托盘</option><option value="minimize">{status.platform==='darwin'?'最小化到程序坞':'最小化到任务栏'}</option><option value="exit">退出程序</option></select></div><div className="settings-actions"><button disabled={busy} onClick={()=>ask('switch')}>切换卡密</button><button onClick={()=>setModal({title:'Superkiro',detail:`版本 ${status.app_version||'待查询'}`,label:'知道了'})}>版本信息</button><button disabled={busy} onClick={()=>void perform(async()=>{await native('open_external',['https://kiro.rent/#downloads']);})}>下载新版 ↗</button><button disabled={busy||!status.tray_available} onClick={()=>void native('close').catch(e=>captureError(e))}>收至托盘</button><button disabled={busy} onClick={()=>ask('exit')}>退出程序</button><button className="danger-action" disabled={busy} onClick={()=>ask('unbind')}>解除设备绑定</button></div></div></section>}
    </main></div>{modal&&<Confirm value={modal} close={()=>setModal(null)} accept={c=>modal.action?void execute(modal.action,c):setModal(null)}/>}
  </div>;
}








import {adjustmentPointsToMicro} from './pricing';
import ProviderKeyEditor from './ProviderKeyEditor';
import CommercialEditor from './CommercialEditor';
import FinancialPanel from './FinancialPanel';
import {estimatedMoney,financialEstimates} from './financial';
import {loadAdjustment,saveAdjustment,clearAdjustment,isZeroMicroAdjustment,isUnsubmittedAdjustmentRejection,type Adjustment} from './adjustment';
import { useState, useEffect, useCallback, useRef } from 'react';
import { adminApi, AdminApiError, AdminStats, AdminCardItem, AdminAnnouncement, AdminFinancials, GeneratedCard } from './api';

const pages = {overview: ['运营概览', '查看请求成功率、积分结算与供应商成本概况。'], cards: ['卡密资产', '管理卡密发放、余额、有效期和设备绑定。'], groups: ['分组与权益', '配置模型与计费分组、用量上限及关联价格表。'], providers: ['供应商与 Key', '管理供应商连接、API 密钥及可用模型。'], models: ['模型与定价', '配置模型路由、访问权限和计费价格。'], traces: ['调用追踪', '定位失败原因，不记录用户提示词或上游响应正文。'], reconciliation: ['财务对账', '结算积分、估算成本、实际充值分别呈现。'], announcements: ['公告管理', '创建和发布面向全部用户的服务公告。'], security: ['安全与审计', '管理登录会话，查看认证配置和配置发布记录。']} as const;
const tiers = [{"id": "tier-1000", "name": "PRO", "points": 1000, "price_cny": 30}, {"id": "tier-2000", "name": "PRO+", "points": 2000, "price_cny": 55}, {"id": "tier-5000", "name": "PRO Max", "points": 5000, "price_cny": 130}, {"id": "tier-10000", "name": "Power", "points": 10000, "price_cny": 250}];

type Tab = 'overview' | 'cards' | 'groups' | 'providers' | 'models' | 'traces' | 'reconciliation' | 'announcements' | 'security';


type AuthState = 'checking' | 'authenticated' | 'unauthenticated';

function ListEmptyState({loading, failed, empty, onRetry}: {loading: boolean; failed?: boolean; empty: string; onRetry: () => void}) {
  return <div className="list-empty" role="status"><p>{loading ? '正在读取，请稍候…' : failed ? '读取失败，暂时无法确认是否有记录。' : empty}</p>{failed && !loading && <button onClick={onRetry}>重新读取数据</button>}</div>;
}

export default function App() {
  const [rechecking,setRechecking]=useState(false),[recheckError,setRecheckError]=useState('');
  const recheckPending=useRef(false);
  const [authState, setAuthState] = useState<AuthState>('checking');
  const [workspaceVersion, setWorkspaceVersion] = useState(0);
  const [username, setUsername] = useState('admin');
  const [password, setPassword] = useState('');
  // Uncontrolled: React copies a controlled input's value into its DOM attribute, where
  // markup snapshots and attribute selectors can read it. The DOM property is enough.
  const passwordInput = useRef<HTMLInputElement>(null);
  useEffect(() => {if (!password && passwordInput.current) passwordInput.current.value = '';}, [password]);
  const [totpCode,setTotpCode]=useState('');
  const [totpRequired,setTotpRequired]=useState(false);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState('');
  const [expiring, setExpiring] = useState(false);
  const attempt = useRef(0);
  const pending = useRef(false);

  useEffect(() => {
    let current = true;
    const version = ++attempt.current;
    adminApi.onUnauthorized = (reason) => {
      ++attempt.current;
      setPassword('');setTotpCode(''); setAuthState('unauthenticated');setExpiring(false);
      setError(reason === 'expired' ? '会话已到期（每次登录有效 15 分钟），请重新登录。未发布的公告和配置草稿已保留。' : '');
    };
    adminApi.onExpiring = () => {if (current) setExpiring(true);};
    adminApi.onSessionChanged = () => {
      if (current) setWorkspaceVersion(version => version + 1);
    };
    adminApi.checkAuth().then(() => {
      if (current && version === attempt.current) setAuthState('authenticated');
    }).catch(() => {
      if (current && version === attempt.current) {
        adminApi.clearSession(); setAuthState('unauthenticated');
      }
    }).finally(()=>{if(current)setTotpRequired(adminApi.totpRequired);});
    return () => {current = false; ++attempt.current; adminApi.onUnauthorized = undefined; adminApi.onSessionChanged = undefined; adminApi.onExpiring = undefined; adminApi.clearSession();};
  }, []);

  useEffect(()=>{
    if(authState!=='authenticated')return;
    const recheck=()=>{
      if(document.hidden||pending.current||recheckPending.current)return;
      const version=++attempt.current;recheckPending.current=true;setRechecking(true);setRecheckError('');
      void adminApi.checkAuth().then(()=>{if(version===attempt.current)setAuthState('authenticated');}).catch(()=>{if(version===attempt.current){setRecheckError('会话验证未完成，草稿已保留。请重试验证后继续操作。');}}).finally(()=>{recheckPending.current=false;setRechecking(false);});
    };
    window.addEventListener('focus',recheck);document.addEventListener('visibilitychange',recheck);
    return()=>{window.removeEventListener('focus',recheck);document.removeEventListener('visibilitychange',recheck);};
  },[authState]);

  async function login() {
    if (pending.current) return;
    pending.current = true; setBusy(true); setError('');
    const version = ++attempt.current;
    try {
      await adminApi.establishSession(username.trim(), password, totpCode || undefined);
      if (version === attempt.current) {setPassword('');setTotpCode('');setRecheckError('');setExpiring(false); setAuthState('authenticated');}
    } catch (cause) {
      if (version === attempt.current) {
        adminApi.clearSession(); setError(cause instanceof Error ? cause.message : '登录失败，请重试');
      }
    } finally {pending.current = false; setBusy(false); setPassword('');setTotpCode('');setTotpRequired(adminApi.totpRequired);}
  }

  async function logout(all: boolean) {
    if (all && !window.confirm('确认撤销所有管理员会话？所有已登录管理员都需要重新登录。')) return;
    if (pending.current) return;
    const version = ++attempt.current;
    pending.current = true; setBusy(true); setPassword('');setTotpCode(''); setError('');
    // logout clears the API session synchronously, before awaiting the server.
    const revocation = adminApi.logout(all);
    setAuthState('unauthenticated');
    try {await revocation;}
    catch {if (version === attempt.current) setError('本地已退出，但服务端会话撤销未确认，请关闭浏览器以结束本次使用。');}
    finally {pending.current = false; setBusy(false);}
  }

  if (authState === 'checking') return <main className="auth-page"><p role="status">正在检查会话…</p></main>;
  if (authState === 'authenticated') return <>{expiring&&<div className="fixed inset-x-0 top-0 z-[110] bg-amber-100 text-amber-900 text-sm px-4 py-2 text-center" role="alert">会话将在约 2 分钟后到期。到期后需要重新登录；未发布的公告和配置草稿会保留，请先完成正在进行的操作。</div>}<div ref={node=>{if(node)node.inert=rechecking||!!recheckError;}} aria-hidden={rechecking||!!recheckError||undefined}><AdminWorkspace key={workspaceVersion} onLogout={logout} operator={adminApi.authenticatedUsername} onReauthenticate={()=>{++attempt.current;adminApi.clearSession();setAuthState('unauthenticated');setError('请重新登录确认调账账户，未确认意图不会自动提交。');}} /></div>{(rechecking||recheckError)&&<div className="fixed inset-0 z-[100] bg-white/80 flex items-center justify-center" role="status" aria-live="polite">{rechecking?'正在检查会话…':<section><p>{recheckError}</p><button onClick={()=>window.dispatchEvent(new Event('focus'))}>重新验证会话</button></section>}</div>}</>;
  return <main className="auth-page"><section className="auth-card" aria-labelledby="login-title">
    <p className="auth-brand">Superkiro</p><h1 id="login-title">管理员登录</h1>
    <form onSubmit={event => {event.preventDefault(); void login();}} aria-busy={busy}>
      <label>用户名<input autoComplete="username" required disabled={busy} value={username} onChange={event => setUsername(event.target.value)} /></label>
      <label>密码<input ref={passwordInput} type="password" autoComplete="current-password" required disabled={busy} onChange={event => setPassword(event.target.value)} /></label>
      {totpRequired && <><label>动态验证码{totpRequired?'（必填）':'（已配置 2FA 时填写）'}<input aria-label="动态验证码" inputMode="numeric" autoComplete="one-time-code" pattern="[0-9]{6}" maxLength={6} required={totpRequired} disabled={busy} value={totpCode} onChange={event=>setTotpCode(event.target.value)} /></label>
      <p className="muted">验证码已使用或服务刚重启时，请等待下一轮动态码再试。</p></>}
      {error && <p role="alert">{error}</p>}
      <button className="primary" type="submit" disabled={busy || !username.trim() || !password}>{busy ? '请稍候…' : '登录'}</button>
    </form>
  </section></main>;
}

function AdminWorkspace({onLogout: handleLogout,operator,onReauthenticate}: {onLogout: (all: boolean) => Promise<void>;operator:string|null;onReauthenticate:()=>void}) {
  const isAuthenticated = true;
  const mounted = useRef(true);
  useEffect(() => {mounted.current = true; return () => {mounted.current = false;};}, []);
  const [activeTab, setActiveTab] = useState<Tab>('overview');
  const commercialDirty=useRef(false);
  const providerDirty=useRef(false);
  const editorBusy=useRef(false);
  const markProviderDirty=useCallback((dirty:boolean)=>{providerDirty.current=dirty;},[]);
  const markEditorBusy=useCallback((busy:boolean)=>{editorBusy.current=busy;},[]);
  const mayLeaveProvider=()=>{if(editorBusy.current){showToast('操作正在处理中，请等待结果后再切换页面。');return false;}return !providerDirty.current || window.confirm('离开将丢弃未保存的供应商密钥草稿，继续吗？');};
  useEffect(()=>{const guard=(event:BeforeUnloadEvent)=>{if(commercialDirty.current||providerDirty.current||editorBusy.current){event.preventDefault();event.returnValue='';}};window.addEventListener('beforeunload',guard);return()=>window.removeEventListener('beforeunload',guard);},[]);
  const markCommercialDirty=useCallback((dirty:boolean)=>{commercialDirty.current=dirty;},[]);
  const navigate=(tab:Tab)=>{if(cardBulkBusy)return;if(tab===activeTab)return;if(!mayLeaveProvider())return;if(commercialDirty.current&&!window.confirm('离开将丢弃未发布的配置草稿，继续吗？'))return;commercialDirty.current=false;providerDirty.current=false;setActiveTab(tab);};

  // Interactive state
  const [cardStatusFilter, setCardStatusFilter] = useState('CURRENT');
  const [searchQuery, setSearchQuery] = useState('');
  const [cardPage, setCardPage] = useState(0);
  const [selectedCardIds, setSelectedCardIds] = useState<string[]>([]);
  const [cardBulkBusy, setCardBulkBusy] = useState(false);
  const [cardBulkResults, setCardBulkResults] = useState<Array<{id: string; result: string}>>([]);
  const [showBatchModal, setShowBatchModal] = useState(false);
  const [generatedCards, setGeneratedCards] = useState<GeneratedCard[]>([]);
  const issuanceStorageKey = 'admin-pending-issuance:v1';
  const [issuanceRecovery, setIssuanceRecovery] = useState<'refresh' | 'review' | null>(() => {
    try {return sessionStorage.getItem(issuanceStorageKey) ? 'refresh' : null;} catch {return 'refresh';}
  });
  const [issuanceReference, setIssuanceReference] = useState(() => {try{const reference=JSON.parse(sessionStorage.getItem(issuanceStorageKey) || '{}')?.reference;return typeof reference==='string' && reference.length<=256 ? reference : '';}catch{return '';}});
  const [issuanceChecking, setIssuanceChecking] = useState(false);
  const refreshIssuanceForReview = async () => {
    if (issuanceChecking || writing.current) return;
    setIssuanceChecking(true); setIssuanceRecovery('refresh');
    try {
      const result = await adminApi.getCards();
      if (!result.success) throw new Error('服务端未确认卡密列表');
      setCards(result.cards); setIssuanceRecovery('review'); setActionError('');
    } catch {setActionError('卡密核对刷新失败，仍禁止制卡。请重试刷新。');}
    finally {setIssuanceChecking(false);}
  };
  const [showAdjustModal, setShowAdjustModal] = useState(false);
  const [showKekModal, setShowKekModal] = useState(false);
  const [showNoticeModal, setShowNoticeModal] = useState(false);
  const [mutationBusy, setMutationBusy] = useState(false);
  const [providerBusy, setProviderBusy] = useState(false);
  const [pruning, setPruning] = useState(false);
  const [revealedCard, setRevealedCard] = useState<{cardId: string; rawCode: string} | null>(null);
  const [revealing, setRevealing] = useState(false);
  const [actionError, setActionError] = useState('');
  const [toastMessage, setToastMessage] = useState<string | null>(null);
  const toastTimer = useRef<ReturnType<typeof setTimeout>>();
  useEffect(() => () => clearTimeout(toastTimer.current), []);

  // Live data
  const [stats, setStats] = useState<AdminStats | null>(null);
  const [cards, setCards] = useState<AdminCardItem[]>([]);
  const [announcements, setAnnouncements] = useState<AdminAnnouncement[]>([]);
  const [financials, setFinancials] = useState<AdminFinancials | null>(null);
  const [traces, setTraces] = useState<Array<Record<string, unknown>>>([]);
  const successfulTraces = traces.filter(trace => trace.status === 'success').length;
  const completedTraces = traces.filter(trace => ['success', 'error', 'client_aborted'].includes(String(trace.status))).length;
  const traceBins = Array.from({length: 12}, (_, hour) => traces.filter(trace => {
    const date = new Date(Number(trace.ts) * 1000);
    return Number.isFinite(date.getTime()) && Math.floor(date.getHours() / 2) === hour;
  }).length);
  const [providers, setProviders] = useState<Array<Record<string, unknown>>>([]);
  const [selectedProviderKey, setSelectedProviderKey] = useState<Record<string, unknown> | undefined>();
  const selectProviderKey = (key?: Record<string, unknown>) => {
    if (key?.id !== selectedProviderKey?.id || key?.provider_id !== selectedProviderKey?.provider_id) {
      if (!mayLeaveProvider()) return;
      providerDirty.current = false; setSelectedProviderKey(key);
    }
    document.getElementById('key-editor')?.scrollIntoView({behavior: 'smooth'});
  };
  const [providerKeys, setProviderKeys] = useState<Array<Record<string, unknown>>>([]);
  const [loading, setLoading] = useState(true);
  const [dataFailures, setDataFailures] = useState<Record<string, boolean>>({});
  const [syncedAt, setSyncedAt] = useState<string | null>(null);
  const [loadError, setLoadError] = useState('');
  const [groupFilter, setGroupFilter] = useState('ALL');
  const [traceQuery, setTraceQuery] = useState('');
  const [tracePage, setTracePage] = useState(0);
  const [traceStatusFilter, setTraceStatusFilter] = useState('ALL');
  const traceStatusLabel = (value: unknown) => ({success: '成功', error: '失败', client_aborted: '客户端中断', pending: '处理中', running: '处理中', in_progress: '处理中'}[String(value)] ?? String(value ?? '未知'));
  const filteredTraces = traces.filter(trace =>
    (traceStatusFilter === 'ALL' || (traceStatusFilter === 'in_progress' ? ['pending', 'running', 'in_progress'].includes(String(trace.status)) : trace.status === traceStatusFilter)) &&
    (!traceQuery.trim() || [trace.id, trace.card_id, trace.exposed_model, trace.status, traceStatusLabel(trace.status)].some(value => String(value ?? '').toLowerCase().includes(traceQuery.trim().toLowerCase()))));
  const traceFiltersChanged = !!traceQuery || traceStatusFilter !== 'ALL';
  const resetTraceFilters = () => {setTraceQuery(''); setTraceStatusFilter('ALL'); setTracePage(0);};
  const tracePageCount = Math.max(1, Math.ceil(filteredTraces.length / 20));
  const currentTracePage = Math.min(tracePage, tracePageCount - 1);
  const [selectedTrace, setSelectedTrace] = useState<Record<string, unknown> | null>(null);
  const [audit, setAudit] = useState<Array<Record<string, unknown>>>([]);
  const [auditError, setAuditError] = useState('');
  const [auditLoading, setAuditLoading] = useState(false);
  const [providersLoaded, setProvidersLoaded] = useState(false);
  useEffect(() => {
    if (activeTab !== 'security' || !isAuthenticated) {setAudit([]); return;}
    if (loading) return;
    let current = true; setAuditLoading(true);
    adminApi.getCommercialConfig().then(result => {if (!result.success) throw new Error('未确认配置审计'); if (current) {setAudit(result.config.audit); setAuditError('');}}).catch(() => {if (current) setAuditError('配置审计读取失败，已有记录可能不是最新结果。');}).finally(() => {if (current) setAuditLoading(false);});
    return () => {current = false;};
  }, [activeTab, isAuthenticated, loading]);

  // Selected card for adjustment
  const [selectedCard, setSelectedCard] = useState<AdminCardItem | null>(null);
  const [adjustAmount, setAdjustAmount] = useState<string>('10');
  const [adjustReason, setAdjustReason] = useState<string>('');
  const adjustment=useRef<Adjustment|null>(null);
  useEffect(()=>{
    adjustment.current=null;if(!operator)return;
    try{adjustment.current=loadAdjustment(sessionStorage,operator);if(adjustment.current)setActionError(`卡 ${adjustment.current.cardId} 有未确认调账。请在卡密资产中打开原卡核对，再手动确认重试；未自动提交。`);}
    catch{setActionError('无法读取保存的调账意图，请检查浏览器存储并人工核对账本；暂不允许新调账。');}
  },[operator]);
  const adjusting=useRef(false), writing=useRef(false);

  // Batch generation form
  const [batchCount, setBatchCount] = useState('50');
  const batchCountValue = Number(batchCount);
  const validBatchCount = /^\d+$/.test(batchCount) && Number.isInteger(batchCountValue) && batchCountValue >= 1 && batchCountValue <= 500;
  const [batchGroup, setBatchGroup] = useState<string>('');
  const [batchTemplate, setBatchTemplate] = useState('tier-2000');
  const [cardGroups, setCardGroups] = useState<Array<Record<string, unknown>>>([]);

  // New announcement form
  const noticeDraftKey = 'admin-announcement-draft:v1';
  const [savedNotice] = useState<Record<string, unknown>>(() => {try {const value = JSON.parse(sessionStorage.getItem(noticeDraftKey) || '{}'); return value && typeof value === 'object' ? value : {};} catch {return {};}});
  const [noticeTitle, setNoticeTitle] = useState<string>(() => typeof savedNotice.title === 'string' ? savedNotice.title : '');
  const [noticeLevel, setNoticeLevel] = useState<'info' | 'warning' | 'critical'>(() => savedNotice.level === 'warning' || savedNotice.level === 'critical' ? savedNotice.level : 'info');
  const [noticeContent, setNoticeContent] = useState<string>(() => typeof savedNotice.content === 'string' ? savedNotice.content : '');
  useEffect(() => {
    try {
      if (noticeTitle || noticeContent) sessionStorage.setItem(noticeDraftKey, JSON.stringify({title: noticeTitle, level: noticeLevel, content: noticeContent}));
      else sessionStorage.removeItem(noticeDraftKey);
    } catch {/* A draft that cannot be kept is only lost at a session end. */}
  }, [noticeTitle, noticeLevel, noticeContent]);
  const noticeStorageKey = 'admin-pending-announcement:v1';
  const [noticeRecovery, setNoticeRecovery] = useState<'refresh' | 'review' | null>(() => {
    try {return sessionStorage.getItem(noticeStorageKey) ? 'refresh' : null;} catch {return 'refresh';}
  });
  const [noticeChecking, setNoticeChecking] = useState(false);
  const checkingNotices = useRef(false);

  const showToast = (msg: string) => {
    clearTimeout(toastTimer.current);
    setToastMessage(msg);
    toastTimer.current = setTimeout(() => setToastMessage(null), 5000);
  };

  const refreshData = useCallback(async (preserveSelection = false) => {
    if (!mounted.current) return;
    try {
      setLoading(true);
      if (!preserveSelection) setSelectedCardIds([]);
      setLoadError('');
      const failures: string[] = [];
      const failed = (section: string, error: unknown) => {failures.push(`${section}：${error instanceof Error ? error.message : String(error)}`);};
      const [statsRes, cardsRes, noticesRes, financialsRes, tracesRes, providersRes, configRes] = await Promise.all([
        adminApi.getStats().catch(e => {failed('统计', e); return null;}),
        adminApi.getCards().catch(e => {failed('卡密', e); return { success: false, count: 0, cards: [] };}),
        adminApi.getAnnouncements().catch(e => {failed('公告', e); return { success: false, announcements: [] };}),
        adminApi.getFinancials().catch(e => {failed('财务', e); return null;}),
        adminApi.getTraces(100).catch(e => {failed('追踪', e); return { success: false, traces: [] };}),
        adminApi.getProviders().catch(e => {failed('供应商', e); return { success: false, providers: [], keys: [] };}),
        adminApi.getCommercialConfig().catch(e => {failed('配置', e); return {success: false, config: null};}),
      ]);

      if (!mounted.current) return;
      const unavailable = {stats: !statsRes?.success, cards: !cardsRes.success, announcements: !noticesRes.success, financials: !financialsRes?.success, traces: !tracesRes.success, providers: !providersRes.success, config: !configRes.success};
      setDataFailures(unavailable);
      if (Object.values(unavailable).some(Boolean)) setLoadError(`部分数据读取失败。已有结果保留上次读取内容，不代表最新状态；空白不表示没有记录。${failures.join('；')}`);
      else setSyncedAt(new Date().toLocaleTimeString([], {hour: '2-digit', minute: '2-digit'}));
      if (configRes.success && configRes.config) {
        const groups = configRes.config.groups; setCardGroups(groups);
        const issuable = groups.filter(group => group.issuance_enabled !== false);
        setBatchGroup(current => issuable.some(group => group.id === current) ? current : issuable.length === 1 ? String(issuable[0].id) : '');
      } else { setCardGroups([]); setBatchGroup(''); }
      if (statsRes && statsRes.success) {
        setStats(statsRes);
      }
      if (cardsRes && cardsRes.success) {
        setCards(cardsRes.cards);
      }
      if (noticesRes && noticesRes.success) {
        setAnnouncements(noticesRes.announcements);
      }
      if (financialsRes && financialsRes.success) setFinancials(financialsRes);
      if (tracesRes && tracesRes.success) setTraces(tracesRes.traces);
      if (providersRes && providersRes.success) {
        setProvidersLoaded(true);
        setProviders(providersRes.providers || []);
        setProviderKeys(providersRes.keys || []);
      }
    } catch (err) {
      console.error('Failed to load admin data:', err);
      if (mounted.current) setLoadError('读取未完成，请重新刷新；当前显示的结果可能不是最新状态。');
    } finally {
      setLoading(false);
    }
  }, []);

  useEffect(() => {void refreshData();}, [refreshData]);

  useEffect(() => {
    if (!(showBatchModal || showAdjustModal || showKekModal || showNoticeModal || revealedCard || generatedCards.length)) return;
    const previous = document.activeElement as HTMLElement | null;
    const dialog = document.querySelector<HTMLElement>('[role="dialog"]');
    const focusable = () => Array.from(dialog?.querySelectorAll<HTMLElement>('button:not([disabled]), input:not([disabled]), select:not([disabled]), textarea:not([disabled]), [tabindex="0"]') || []);
    const background = Array.from(document.querySelectorAll<HTMLElement>('.admin-app > aside, .admin-app > main, .admin-app > .skip-link'));
    background.forEach(node => {node.inert = true;});
    const overflow = document.body.style.overflow;
    document.body.style.overflow = 'hidden';
    (focusable()[0] ?? dialog)?.focus();
    const keydown = (event: KeyboardEvent) => {
      if(event.key==='Escape'){const close=Array.from(dialog?.querySelectorAll<HTMLButtonElement>('button:not([disabled])')??[]).find(button=>['取消','关闭并清除','确认并清除'].includes(button.textContent??''));if(close){event.preventDefault();close.click();}return;}
      if (event.key !== 'Tab') return;
      const nodes = focusable(); const first = nodes[0]; const last = nodes[nodes.length - 1];
      if (!first) {event.preventDefault(); dialog?.focus(); return;}
      if (!dialog?.contains(document.activeElement)) {event.preventDefault(); first?.focus();}
      else if (event.shiftKey && document.activeElement === first) {event.preventDefault(); last?.focus();}
      else if (!event.shiftKey && document.activeElement === last) {event.preventDefault(); first?.focus();}
    };
    document.addEventListener('keydown', keydown);
    return () => {document.removeEventListener('keydown', keydown); background.forEach(node => {node.inert = false;}); document.body.style.overflow = overflow; previous?.focus();};
  }, [showBatchModal, showAdjustModal, showKekModal, showNoticeModal, revealedCard, generatedCards.length]);

  const handleCardStatus = async (cardId: string, action: 'freeze' | 'unfreeze' | 'ban') => {
    if(writing.current || cardBulkBusy || loading || dataFailures.cards)return;
    const impact = {freeze: '冻结后暂停使用，可通过解冻恢复；不会删除卡密或修改余额。', unfreeze: '解除冻结后仍受有效期和余额限制，不会续期或增加积分。', ban: '封禁后停止使用，本页面不支持解除封禁；不自动退款，请谨慎确认。'}[action];
    if (!window.confirm(`确认对卡密 ${cardId} 执行${action === 'freeze' ? '冻结' : action === 'ban' ? '封禁' : '解冻'}？\n${impact}`)) return;
    try {
      writing.current=true;setMutationBusy(true);setActionError('');const res = await adminApi.updateCardStatus(cardId, action, `管理员手动操作：${{freeze: '冻结', unfreeze: '解冻', ban: '封禁'}[action]}`);
      if (!res.success) throw new Error('服务器未确认状态变更，请刷新核对。');
      if (res.success) {
        showToast(`卡密 ${cardId} 已${action === 'freeze' ? '冻结' : action === 'unfreeze' ? '解冻' : '封禁'}`);
        refreshData();
      }
    } catch (err: any) {
      setActionError(`操作失败：${err.message}`);
    }finally{writing.current=false;setMutationBusy(false);}
  };

  const handleCardBulk = async (action: 'freeze' | 'unfreeze' | 'ban' | 'void' | 'archive' | 'unarchive' | 'export') => {
    if (writing.current || cardBulkBusy || loading || dataFailures.cards || mutationBusy || revealing) return;
    const targets = pageCards.filter(card => selectedCardIds.includes(card.id));
    if (!targets.length) return;
    if (action === 'void') {
      if (!operator) {setActionError('请先重新登录确认调账账户，再删除卡密；本次未发送删除请求。'); return;}
      try {
        const pending = operator ? loadAdjustment(sessionStorage, operator) : null;
        if (pending && targets.some(card => card.id === pending.cardId)) {
          setActionError(`卡 ${pending.cardId} 有未确认调账，请先核对原调账，再删除；本次未发送删除请求。`); return;
        }
      } catch {setActionError('调账恢复记录无法读取，请先核对账本；本次未发送删除请求。'); return;}
    }
    const label = {freeze: '冻结', unfreeze: '解冻', ban: '封禁', void: '删除（永久作废）', archive: '归档', unarchive: '取消归档', export: '导出明文卡密'}[action];
    // Name what is about to change: a count alone let a mis-ticked page be voided.
    const named = targets.slice(0, 8).map(card => card.id).join('、') + (targets.length > 8 ? ` 等 ${targets.length} 张` : '');
    const activated = targets.filter(card => card.status !== 'unactivated').length;
    const withBalance = targets.filter(card => card.creditTotal > card.creditUsed).length;
    if (!window.confirm(`确认仅对已选 ${targets.length} 张卡密执行${label}？
${named}
其中已激活 ${activated} 张，有剩余额度 ${withBalance} 张。
${action === 'export' ? '下载文件包含秘密，请妥善保管。' : action === 'void' ? (activated ? '含已激活卡密；' : '') + '删除后不可恢复或使用，剩余额度失效但账面余额、财务与审计记录保留，不自动退款。有在途请求的卡将拒绝删除，请先冻结并等待结算后重试。' : action === 'archive' || action === 'unarchive' ? '仅改变管理列表展示，不解封、不续期、不修改余额或历史账本。只有已封禁、已到期或已作废的卡密可归档。' : '状态操作可能中断使用，请核对选择。'}`)) return;
    writing.current = true; setCardBulkBusy(true); setCardBulkResults([]); setActionError('');
    const results: Array<{id: string; result: string}> = [];
    const codes: string[] = [];
    const failedIds: string[] = [];
    try {
      for (const card of targets) {
        if (!mounted.current) break;
        try {
          if (action === 'export') {
            if (!card.codeRecoverable) {failedIds.push(card.id); results.push({id: card.id, result: '失败：历史卡密不可恢复'}); continue;}
            const response = await adminApi.revealCard(card.id);
            if (!mounted.current) break;
            if (!response.success || !response.rawCode) throw new Error('reveal failed');
            codes.push(response.rawCode);
          } else {
            if ((action === 'archive' && (card.archivedAt != null || !(['banned', 'expired', 'voided'].includes(card.status) || (card.validUntil != null && card.validUntil <= Date.now() / 1000)))) || (action === 'unarchive' && card.archivedAt == null)) {
              failedIds.push(card.id); results.push({id: card.id, result: '未执行：不符合归档条件或已是目标状态'}); continue;
            }
            if ((action === 'freeze' && card.status !== 'active') || (action === 'unfreeze' && card.status !== 'frozen') || (action === 'ban' && ['banned', 'voided'].includes(card.status)) || (action === 'void' && card.status === 'voided')) {
              failedIds.push(card.id); results.push({id: card.id, result: '未执行：当前状态不适用该操作'}); continue;
            }
            const response = await adminApi.updateCardStatus(card.id, action, `管理员批量操作：${label}`);
            if (!response.success) throw new Error('status failed');
          }
          results.push({id: card.id, result: action === 'export' ? '读取成功' : `${label}成功`});
        } catch (error) {
          failedIds.push(card.id);
          // Do not retain server error bodies in a secret-bearing operation.
          results.push({id: card.id, result: action === 'export' ? '读取失败，请核对会话或卡密可恢复性' : '失败或结果未确认，请刷新核对后再操作'});
          if (error instanceof AdminApiError && (error.status === 401 || error.status === 403)) {
            for (const pending of targets.slice(results.length)) {failedIds.push(pending.id); results.push({id: pending.id, result: '未执行：管理会话或权限失效'});}
            if (action === 'export') failedIds.push(...targets.map(card => card.id));
            codes.length = 0; break;
          }
        }
        if (mounted.current) setCardBulkResults([...results]);
      }
      if (mounted.current && action === 'export' && codes.length) {
        const url = URL.createObjectURL(new Blob([codes.join('\n')], {type: 'text/plain;charset=utf-8'}));
        try {
          const link = document.createElement('a'); link.href = url;
          link.download = `selected-cards-${new Date().toISOString().slice(0, 10)}.txt`;
          link.click();
          showToast(`已发起下载 ${codes.length} 张卡密，请核对下载文件`);
        } finally {setTimeout(() => URL.revokeObjectURL(url), 1000);}
      }
    } catch {
      failedIds.push(...targets.map(card => card.id));
      if (mounted.current) setActionError('导出文件创建失败，未确认下载成功。请重新选择后重试。');
    } finally {
      codes.length = 0;
      if (mounted.current) {
        setCardBulkResults(results); setSelectedCardIds([...new Set(failedIds)]);
        await refreshData(true);
        setCardBulkBusy(false);
      }
      writing.current = false;
    }
  };

  const handleAdjustSubmit = async () => {
    if (!selectedCard || mutationBusy || adjusting.current) return;
    if(!operator){setActionError('请先重新登录以确认调账账户。');return;}
    let delta:number;
    if(adjustment.current&&isZeroMicroAdjustment(adjustment.current)){setActionError('该旧意图换算为零微积分，无法入账；请确认清除后更正金额。');return;}
    // Existing nonzero legacy intents must replay the original numeric payload, never round it again.
    try{delta=adjustment.current&&Number(adjustAmount)===adjustment.current.delta?adjustment.current.delta:adjustmentPointsToMicro(adjustAmount)/1_000_000;}catch{setActionError('请输入非零、最多六位小数的调账积分，范围为 ±1,000,000；未发送请求。');return;}
    if (!adjustReason.trim()||adjustReason.trim().length>500) { setActionError('请填写 1–500 字的调账原因，不要包含密码或令牌'); return; }
    if (!window.confirm(`确认调整 ${selectedCard.id} 的积分 ${delta > 0 ? '+' : ''}${delta}？\n原因：${adjustReason.trim()}\n确认后将写入账本；关闭弹窗不会撤销已发送的调账。`)) return;
    let sent=false;
    try {
      adjusting.current=true;setMutationBusy(true); setActionError('');
      const intent=loadAdjustment(sessionStorage,operator)??{operator,cardId:selectedCard.id,delta,reason:adjustReason.trim(),key:crypto.randomUUID()};
      if(intent.cardId!==selectedCard.id||intent.delta!==delta||intent.reason!==adjustReason.trim())throw new Error('上次调账结果尚未确认，请先用原参数重试');
      saveAdjustment(sessionStorage,intent);adjustment.current=intent;sent=true;
      const res = await adminApi.adjustBalance(intent.cardId,intent.delta,intent.reason,intent.key);
      if(!res.success)throw new Error('服务端未确认调账');
      if (res.success) {
        clearAdjustment(sessionStorage,intent);adjustment.current=null;showToast(`卡密 ${selectedCard.id} 调账成功`);
        setShowAdjustModal(false);
        setSelectedCard(null);
        setAdjustReason('');
        refreshData();
      }
    } catch (err: any) {
      if(sent&&adjustment.current&&err instanceof AdminApiError&&isUnsubmittedAdjustmentRejection(err.status,err.message,adjustment.current)){
        try{clearAdjustment(sessionStorage,adjustment.current);adjustment.current=null;setActionError('服务端明确拒绝调账，未入账；已清除该无效意图，请检查卡密状态、账面余额与调账金额。');return;}catch{setActionError('该调账未入账，但本地记录未能清除，请检查浏览器存储。');return;}
      }
      setActionError(`${sent?'调账结果未确认':'尚未发送调账'}：${err.message}。请核对原意图；重试仍需手动确认。`);
    } finally {adjusting.current=false;setMutationBusy(false);}
  };

  const issuableGroups = cardGroups.filter(group => group.issuance_enabled !== false);
  const selectedTier = tiers.find(tier => tier.id === batchTemplate);
  const selectedGroup = issuableGroups.find(group => group.id === batchGroup);
  const issuanceSummary = `套餐：${selectedTier?.name ?? '未选择'} · ${selectedTier?.points.toLocaleString() ?? '—'} 积分 · 有效期 30 天 · 模型与计费分组：${String(selectedGroup?.name ?? selectedGroup?.id ?? '未选择')}`;

  const handleBatchGenerate = async () => {
    if (issuanceRecovery || writing.current || loading || !tiers.some(t => t.id === batchTemplate) || !selectedGroup || !validBatchCount) return;
    if (!window.confirm(`确认生成 ${batchCount} 张 卡密？\n${issuanceSummary}\n每张仅限一台设备，积分与权益以服务端校验为准。`)) return;
    try {
      writing.current=true;setLoading(true);
      const reference = `批次 ${new Date().toISOString()} ${crypto.randomUUID()}`;
      setIssuanceReference(reference);
      sessionStorage.setItem(issuanceStorageKey, JSON.stringify({reference, count: batchCountValue, group: batchGroup, template: batchTemplate, startedAt: new Date().toISOString()}));
      const res = await adminApi.batchCards(batchCountValue, batchGroup, batchTemplate, reference);
      if (!res.success) throw new Error('服务器未确认生成结果');
      if (res.success) {
        showToast(`成功批量生成 ${res.cards.length} 张卡密！已持久化入库`);
        setGeneratedCards(res.cards);
        sessionStorage.removeItem(issuanceStorageKey);
        setShowBatchModal(false);
        await refreshData();
      }
    } catch (err: any) {
      if (refused(err)) {sessionStorage.removeItem(issuanceStorageKey); setActionError(`服务端已拒绝，未生成卡密：${err.message}`); return;}
      setIssuanceRecovery('refresh'); setShowBatchModal(false);
      setActionError(`批量制卡结果未确认：${err.message}。请先核对卡密列表，不要立即重复生成。`);
    } finally {
      writing.current=false;setLoading(false);
    }
  };

  const downloadGeneratedCards = () => {
    if (!generatedCards.length) { showToast('仅可导出刚生成的卡密，请先批量制卡'); return; }
    const cell = (value: string | number) => {
      const text = String(value);
      return `"${(/^[=+@\-\t\r]/.test(text) ? "'" + text : text).replace(/"/g, '""')}"`;
    };
    const csv = ['cardId,rawCode,groupId,creditTotal', ...generatedCards.map(card =>
      [card.cardId, card.rawCode, card.groupId, card.creditTotal].map(cell).join(','))].join('\r\n');
    const url = URL.createObjectURL(new Blob(['\uFEFF' + csv], { type: 'text/csv;charset=utf-8' }));
    const link = document.createElement('a');
    link.href = url;
    link.download = `generated-cards-${new Date().toISOString().slice(0, 10)}.csv`;
    document.body.appendChild(link);
    link.click();
    link.remove();
    setTimeout(() => URL.revokeObjectURL(url), 1000);
  };

  const clearGeneratedCards = () => {
    if (window.confirm('关闭后本页面不能再次显示这些卡密，确认已完成交付？')) setGeneratedCards([]);
  };

  const handleToggleProvider = async (providerId: string, currentEnabled: boolean) => {
    if (writing.current || loading || dataFailures.providers) return;
    if (!window.confirm(`确认${currentEnabled ? '停用' : '启用'}供应商 ${providerId}？这会影响该供应商的模型路由。`)) return;
    writing.current = true; setProviderBusy(true); setActionError('');
    try {
      const res = await adminApi.updateProviderStatus(providerId, !currentEnabled);
      if (!res.success) throw new Error('服务端未确认状态变更');
      if (res.success) {
        showToast(`供应商 ${providerId} 状态已更新为: ${!currentEnabled ? '已启用' : '已停用'}`);
        await refreshData();
      }
    } catch (err: any) {
      setActionError(`切换结果未确认：${err.message}。请刷新核对供应商状态后再操作。`);
    } finally {writing.current = false; setProviderBusy(false);}
  };

  const exporting = useRef(false);
  const handleExportLedger = async (format: 'json' | 'csv') => {
    if (exporting.current) return;
    exporting.current = true;
    try {
      showToast(`正在导出 ${format.toUpperCase()} 对账账本...`);
      const blob = await adminApi.exportLedger(format);
      const url = window.URL.createObjectURL(blob);
      const a = document.createElement('a');
      a.href = url;
      a.download = `ledger-export-${new Date().toISOString().slice(0, 10)}.${format}`;
      document.body.appendChild(a);
      a.click();
      a.remove();
      // The browser reads the file after click() returns; revoking at once can cut it.
      setTimeout(() => window.URL.revokeObjectURL(url), 60000);
      showToast(`对账 ${format.toUpperCase()} 已开始下载，请在浏览器下载列表中确认文件完整。`);
    } catch (err: any) {
      setActionError(`导出失败：${err.message}`);
    } finally {exporting.current = false;}
  };

  const handlePruneTraces = async () => {
    if (writing.current || pruning) return;
    if (!window.confirm('确认永久清理 30 天以前的调用追踪？清理后无法在后台查看这些追踪，请先完成审计与备份。')) return;
    writing.current = true; setPruning(true); setActionError('');
    try {
      const result = await adminApi.pruneTraces(Math.floor(Date.now() / 1000) - 30 * 86400);
      if (!result.success) throw new Error('服务端未确认清理结果');
      showToast(`已清理 ${result.pruned} 条调用追踪`);
      await refreshData();
    } catch (err) {setActionError(`清理结果未确认：${err instanceof Error ? err.message : String(err)}。请刷新核对，勿重复提交。`);}
    finally {writing.current = false; setPruning(false);}
  };

  const refreshNoticesForReview = async () => {
    if (checkingNotices.current) return;
    checkingNotices.current = true; setNoticeChecking(true); setNoticeRecovery('refresh');
    try {
      const result = await adminApi.getAnnouncements();
      if (!result.success) throw new Error('服务端未确认公告列表');
      if (!mounted.current) return;
      setAnnouncements(result.announcements); setNoticeRecovery('review'); setActionError('');
    } catch {
      if (mounted.current) setActionError('公告核对刷新失败，仍禁止发布。请重试刷新，不要重复提交公告。');
    } finally {checkingNotices.current = false; if (mounted.current) setNoticeChecking(false);}
  };

  const handleCreateNotice = async () => {
    if (mutationBusy||writing.current||noticeRecovery) return;
    if (!noticeTitle.trim() || !noticeContent.trim()) {
      setActionError('公告标题和内容不能为空');
      return;
    }
    if ([...noticeTitle.trim()].length > 256 || [...noticeContent.trim()].length > 20000) {setActionError('公告标题最多 256 字，正文最多 20,000 字。'); return;}
    if (!window.confirm('确认向全部用户发布该公告？有效期为 7 天。')) return;
    try {
      setMutationBusy(true); setActionError('');
      writing.current=true;sessionStorage.setItem(noticeStorageKey, new Date().toISOString());const res = await adminApi.createAnnouncement(noticeTitle.trim(), noticeContent.trim(), noticeLevel, 86400 * 7);
      if (!res.success) throw new Error('服务端未确认公告发布');
      if (res.success) {
        sessionStorage.removeItem(noticeStorageKey);
        showToast('公告已成功发布并同步到网关');
        setShowNoticeModal(false);
        setNoticeTitle('');
        setNoticeContent('');
        refreshData();
      }
    } catch (err: any) {
      if (refused(err)) {sessionStorage.removeItem(noticeStorageKey); setActionError(`服务端已拒绝，公告未发布：${err.message}`); return;}
      setNoticeRecovery('refresh'); setShowNoticeModal(false);
      setActionError(`公告发布结果未确认：${err.message}。可能已发布，请先刷新并核对现有公告，不要重复提交。`);
    } finally {writing.current=false;setMutationBusy(false);}
  };

  const handleWithdrawNotice = async (notice: AdminAnnouncement) => {
    if (mutationBusy || writing.current) return;
    if (!window.confirm(`确认撤回公告「${notice.title}」？撤回后客户端不再显示该公告（已打开的客户端在下次刷新公告时移除）。`)) return;
    try {
      setMutationBusy(true); setActionError(''); writing.current = true;
      const res = await adminApi.withdrawAnnouncement(notice.id);
      if (res.success !== true) throw new Error('服务端未确认撤回');
      showToast(`公告「${notice.title}」已撤回`);
      refreshData();
    } catch (err: any) {
      setActionError(`公告撤回结果未确认：${err.message}。请刷新公告列表核对。`);
    } finally {writing.current = false; setMutationBusy(false);}
  };

  // Refused by the server's validation or policy: nothing was written, so no review is needed.
  const refused = (err: unknown) => err instanceof AdminApiError && [400, 403, 404, 409, 413, 422].includes(err.status);

  useEffect(() => {setSelectedCardIds([]); setCardPage(0);}, [searchQuery, groupFilter, cardStatusFilter, activeTab]);
  useEffect(() => {setSelectedCardIds([]);}, [cardPage]);
  const changeCardFilter = (update: () => void) => {setSelectedCardIds([]); setCardPage(0); update();};
  const filteredCards = cards.filter((c) => {
    if (groupFilter !== 'ALL' && c.groupId !== groupFilter) return false;
    if ((cardStatusFilter === 'CURRENT' && (c.status === 'voided' || c.archivedAt != null)) || (cardStatusFilter === 'ARCHIVED' && c.archivedAt == null) || (!['ALL', 'CURRENT', 'ARCHIVED'].includes(cardStatusFilter) && c.status.toUpperCase() !== cardStatusFilter.toUpperCase())) {
      return false;
    }
    if (searchQuery.trim()) {
      const q = searchQuery.trim().toLowerCase();
      return c.id.toLowerCase().includes(q) || (c.note && c.note.toLowerCase().includes(q)) || c.boundDevices.some(d => d.toLowerCase().includes(q));
    }
    return true;
  });
  const cardFiltersChanged = !!searchQuery || groupFilter !== 'ALL' || cardStatusFilter !== 'CURRENT';
  const resetCardFilters = () => {setSelectedCardIds([]); setSearchQuery(''); setGroupFilter('ALL'); setCardStatusFilter('CURRENT'); setCardPage(0);};
  const pageCount = Math.max(1, Math.ceil(filteredCards.length / 50));
  const currentCardPage = Math.min(cardPage, pageCount - 1);
  const pageCards = filteredCards.slice(currentCardPage * 50, (currentCardPage + 1) * 50);
  const pageCardIds = JSON.stringify(pageCards.map(card => card.id));
  useEffect(() => {
    const visible: string[] = JSON.parse(pageCardIds);
    setSelectedCardIds(ids => ids.filter(id => visible.includes(id)));
  }, [pageCardIds]);


  return (
    <div className="flex h-screen overflow-hidden bg-white text-[#23272B] font-sans text-sm admin-app">
      {/* Toast Notification */}
      {toastMessage && (
        <div role="status" aria-live="polite" className="fixed top-4 right-4 z-[60] px-4 py-2 bg-[#B94B39] text-white rounded-lg shadow-xl border border-[#E9C8C1] text-xs ">
          {toastMessage}
        </div>
      )}

      <a className="skip-link" href="#admin-workspace">跳到工作区</a>
      <aside className="sidebar">
        <div><h1>Superkiro</h1><p className="brand-caption">SUPERKIRO / CONTROL</p>
          <nav aria-label="管理导航">{(Object.keys(pages) as Tab[]).map(id => <button key={id} aria-current={activeTab === id ? 'page' : undefined} onClick={() => navigate(id)}>{pages[id][0]}</button>)}</nav>
        </div>
        <div className="sidebar-footer"><p>管理员 · {isAuthenticated ? '会话有效' : '尚未认证'}</p><button onClick={() => navigate('security')}>安全设置</button><span> / </span><button onClick={() => void handleLogout(false)}>退出</button></div>
      </aside>
      <main className="workspace" id="admin-workspace" tabIndex={-1}>
        <header className="topbar"><span>工作台 / {pages[activeTab][0]}</span><div><span>{syncedAt ? `最近同步 ${syncedAt}` : '尚未同步'}</span><button onClick={() => navigate('security')}>管理会话</button><button disabled={loading || cardBulkBusy} onClick={() => refreshData()}>{loading ? '刷新中…' : '刷新'}</button></div></header>
        <div className="page-content" key={isAuthenticated ? 'authenticated' : 'anonymous'}>
          <div className="page-heading"><div><h2>{pages[activeTab][0]}</h2><p>{pages[activeTab][1]}</p></div>{activeTab === 'overview' && <button disabled title="现有接口仅返回最近追踪，不支持完整日期聚合" className="sample-range">最近样本 · 日期筛选未支持</button>}{activeTab === 'cards' && <button className="primary" disabled={!isAuthenticated} onClick={() => setShowBatchModal(true)}>＋ 批量生成</button>}{activeTab === 'providers' && <button disabled={!isAuthenticated} className="primary" onClick={() => selectProviderKey()}>＋ 添加供应商</button>}</div>
          {!operator&&<p role="status" className="notice-panel">刷新后需重新登录确认账户，才能新建或恢复调账。<button onClick={onReauthenticate}>重新登录确认调账账户</button></p>}
          {actionError && <div role="alert" className="notice-panel">{actionError} <button onClick={() => setActionError('')}>关闭提示</button></div>}{loadError && <div role="alert" className="notice-panel"><p>{loadError}</p><button disabled={loading || cardBulkBusy} onClick={() => void refreshData()}>重新读取数据</button></div>}
          {activeTab === 'overview' && <div className="space-y-6">
            <div className="metric-grid">
              <section className="panel metric"><p>成功请求</p><strong>{traces.length ? successfulTraces.toLocaleString() : '—'}</strong><small>最近 {traces.length} 条追踪样本 · 非全天</small></section>
              <section className="panel metric"><p>请求成功率</p><strong>{completedTraces ? `${(successfulTraces / completedTraces * 100).toFixed(1)}%` : '—'}</strong><small>最近样本：成功 / 已结束 {completedTraces} 条，排除进行中</small></section>
              <section className="panel metric"><p>已结算积分</p><strong>{financials ? (financials.dashboard.total_credits_charged / 1_000_000).toLocaleString() : '—'}</strong><small>仅含已完成结算</small></section>
              <section className="panel metric"><p>已配置采购成本估算</p><strong>{estimatedMoney(financialEstimates(financials)?.configuredProviderCostMicroCny)}</strong><small>仅保留账本；覆盖范围见财务对账</small></section>
            </div>
            <div className="overview-grid"><section className="panel"><h3>请求量</h3><p className="muted">最近读取的 {traces.length} 条调用追踪 · 非全天统计 · 本地时区按两小时合并（可跨日）</p><div className="request-chart" role="img" aria-label={`最近 ${traces.length} 条追踪按本地时段分布`}>{traces.length ? traceBins.map((count, hour) => { return <div className="chart-column" title={`${hour * 2}:00–${hour * 2 + 2}:00 · ${count} 条`} key={hour}><span>{count}</span><div style={{height: `${count / Math.max(1, ...traceBins) * 180}px`}}/><small>{String(hour * 2).padStart(2, '0')}</small></div>; }) : <ListEmptyState loading={loading} failed={dataFailures.traces} empty="暂无调用追踪数据" onRetry={() => void refreshData()} />}</div></section>
            <section className="panel attention"><h3>需要关注</h3><h4>{providersLoaded ? providerKeys.filter(k => Number(k.cooldown_until ?? 0) > Date.now() / 1000).length : '—'} 个 Key 正在冷却</h4><button onClick={() => navigate('providers')}>查看供应商状态 →</button><h4>{stats ? stats.frozenCards + stats.bannedCards : '—'} 张异常卡密</h4><button onClick={() => navigate('cards')}>查看卡密资产 →</button><p className="muted">成本覆盖及面值估算见财务对账，不代表实际毛利。</p></section></div>
            <section className="panel"><h3>服务健康</h3><table><thead><tr><th>服务</th><th>状态</th><th>Key 数量</th><th>操作</th></tr></thead><tbody>{providers.map(p => <tr key={String(p.id)}><td>{String(p.name || p.id)}</td><td>{p.enabled === false ? '已停用' : '已启用 · 健康状态见 Key'}</td><td>{providerKeys.filter(k => k.provider_id === p.id).length}</td><td><button onClick={() => navigate('providers')}>查看 Key →</button></td></tr>)}{!providers.length && <tr><td colSpan={4} className="empty-state"><ListEmptyState loading={loading} failed={dataFailures.providers} empty="尚未配置供应商" onRetry={() => void refreshData()} /></td></tr>}</tbody></table></section>
          </div>}

          {/* TAB 2: CARDS */}
          {activeTab === 'cards' && (
            <div className="space-y-4">
              <div className="card-filters" role="search" aria-label="卡密筛选">
                <div className="filter-fields">
                  <label>搜索卡密<input
                    type="text"
                    disabled={cardBulkBusy} aria-label="搜索卡密" placeholder="搜索卡密 ID / 备注 / 设备标识"
                    value={searchQuery}
                    onChange={(e) => changeCardFilter(() => setSearchQuery(e.target.value))}
                    className="px-3 py-1.5 bg-white border border-[#E5E8E5] rounded text-xs text-[#23272B]"
                  /></label>
                  <label>模型与计费分组<select disabled={cardBulkBusy} aria-label="分组筛选" value={groupFilter} onChange={e => changeCardFilter(() => setGroupFilter(e.target.value))}><option value="ALL">全部分组</option>{Array.from(new Set(cards.map(c => c.groupId))).map(id => <option key={id} value={id}>{String(cardGroups.find(group => group.id === id)?.name ?? id)} · {id}</option>)}</select></label>
                  <label>卡密状态<select disabled={cardBulkBusy} aria-label="状态筛选"
                    value={cardStatusFilter}
                    onChange={(e) => changeCardFilter(() => setCardStatusFilter(e.target.value))}
                    className="px-3 py-1.5 bg-white border border-[#E5E8E5] rounded text-xs text-[#23272B]"
                  >
                    <option value="CURRENT">工作列表（未删除、未归档）</option><option value="ARCHIVED">已归档记录</option><option value="ALL">全部状态（含已删除、已归档）</option>
                    <option value="ACTIVE">已激活</option>
                    <option value="UNACTIVATED">未激活</option>
                    <option value="FROZEN">已冻结</option>
                    <option value="BANNED">已封禁</option><option value="EXPIRED">已到期</option><option value="VOIDED">已删除（作废记录）</option>
                  </select></label>
                </div>
                <div className="flex gap-2">
                  <button
                    disabled={!generatedCards.length} title="仅导出本次生成结果，不批量读取历史明文" onClick={downloadGeneratedCards}
                    className="px-3 py-1.5 bg-[#EFF1EF] hover:bg-[#EFF1EF] text-[#23272B] rounded text-xs border border-[#E5E8E5]"
                  >
                     导出本次生成结果
                  </button>

                </div>
              </div>

              <div className="filter-summary"><p role="status">匹配 {filteredCards.length} / 已读取 {cards.length} 张卡密 · 仅筛选已加载记录{dataFailures.cards ? '（上次读取结果）' : ''}</p>{cardFiltersChanged && <button disabled={cardBulkBusy} onClick={resetCardFilters}>重置卡密筛选</button>}</div>
              {mutationBusy && <p role="status">正在提交卡密操作，请等待结果，勿重复操作…</p>}
              {adjustment.current && <section className="notice-panel" aria-label="未确认调账恢复"><p>卡 {adjustment.current.cardId} 有未确认调账。即使原卡已被删除或归档，也可使用原幂等键核对，不会创建第二笔调账。</p><button disabled={cardBulkBusy || mutationBusy || loading} onClick={() => {
                try {
                  if (!operator) return;
                  const saved = loadAdjustment(sessionStorage, operator);
                  if (!saved) {adjustment.current = null; setActionError('未找到待核对意图，请刷新列表。'); return;}
                  const card = cards.find(item => item.id === saved.cardId);
                  if (!card) {setActionError('未读取到原卡记录，请刷新或人工核对账本；原意图仍保留。'); return;}
                  adjustment.current = saved; setSelectedCard(card); setAdjustAmount(String(saved.delta)); setAdjustReason(saved.reason); setShowAdjustModal(true);
                } catch {setActionError('调账恢复记录无法读取，请人工核对账本；未发送请求。');}
              }}>核对未确认调账</button></section>}
              <section className="panel bulk-toolbar" aria-label="批量卡密管理">
                <p>已选 {selectedCardIds.length} 张卡密 · 批量操作仅针对当前页所选卡密。</p>
                <div className="actions">
                  <button disabled={cardBulkBusy || !!dataFailures.cards || loading || !pageCards.length} onClick={() => setSelectedCardIds(pageCards.map(card => card.id))}>当前页全选</button>
                  <button disabled={cardBulkBusy || !selectedCardIds.length} onClick={() => setSelectedCardIds([])}>清空选择</button>
                  {(['freeze', 'unfreeze', 'export', 'void'] as const).map(action => <button className={action === 'void' ? 'danger' : action === 'export' ? 'primary' : 'secondary'} key={action} disabled={cardBulkBusy || !!dataFailures.cards || loading || mutationBusy || revealing || !selectedCardIds.length} onClick={() => void handleCardBulk(action)}>{{freeze: '批量冻结', unfreeze: '批量解冻', ban: '批量封禁', void: '批量删除（永久作废）', archive: '批量归档', unarchive: '取消归档', export: '导出已选卡密'}[action]}</button>)}
                  <details className="bulk-more"><summary>更多操作</summary><div className="actions">{(['archive', 'unarchive', 'ban'] as const).map(action => <button className={action === 'ban' ? 'danger' : 'secondary'} key={action} disabled={cardBulkBusy || !!dataFailures.cards || loading || mutationBusy || revealing || !selectedCardIds.length} onClick={() => void handleCardBulk(action)}>{{freeze: '批量冻结', unfreeze: '批量解冻', ban: '批量封禁', void: '批量删除（永久作废）', archive: '批量归档', unarchive: '取消归档', export: '导出已选卡密'}[action]}</button>)}</div><p className="muted">封禁与永久作废会撤销使用权限；作废不自动退款。</p></details>
                </div>
                {cardBulkBusy && <p role="status">正在逐项处理，请勿重复提交…</p>}
                {!!cardBulkResults.length && <div aria-label="批量操作结果" role="status"><p>本次逐项结果（不含卡密明文）：</p>{cardBulkResults.map(item => <p key={item.id}>{item.id}：{item.result}</p>)}</div>}
                <details className="bulk-help"><summary>操作规则与导出安全</summary><p className="muted">删除支持已激活卡密，永久撤销使用权限并保留财务记录，不自动退款。有在途请求时请先冻结，等待结算后重试。仅需清理列表可归档已封禁或到期记录，取消归档不会恢复授权。列表与总量统计保留已作废、归档卡的账面余额，不代表可消费积分。导出文件包含完整卡密，请妥善保管。筛选、翻页或刷新后需重新选择。</p></details>
              </section>
              <p className="table-hint">左右滑动表格查看全部信息和操作。</p>
              <table className="w-full text-left border-collapse bg-white rounded-xl border border-[#E5E8E5] overflow-hidden">
                <thead className="bg-[#EFF1EF] text-xs text-[#7B8388]">
                  <tr>
                    <th className="p-3">选择</th><th className="p-3">卡密 ID</th>
                    <th className="p-3">所属分组</th>
                    <th className="p-3">总积分 / 账面余额</th>
                    <th className="p-3">到期时间 / 设备绑定</th>
                    <th className="p-3">状态</th>
                    <th className="p-3">操作</th>
                  </tr>
                </thead>
                <tbody className="divide-y divide-slate-800 text-xs text-[#23272B]">
                  {filteredCards.length > 0 ? (
                    pageCards.map((card) => (
                      <tr key={card.id}>
                        <td><input type="checkbox" aria-label={`选择卡密 ${card.id}`} disabled={cardBulkBusy || !!dataFailures.cards || loading} checked={selectedCardIds.includes(card.id)} onChange={e => {const checked = e.currentTarget.checked; setSelectedCardIds(ids => checked ? [...ids, card.id] : ids.filter(id => id !== card.id));}} /></td>
                        <td className="p-3 font-mono text-[#475467]">{card.id}{card.note && <small className="card-note">{card.note}</small>}</td>
                        <td className="p-3" title={card.groupId}>{String(cardGroups.find(group => group.id === card.groupId)?.name ?? card.groupId)}</td>
                        <td className="p-3">
                          {card.pointsTotal.toFixed(2)} / {card.pointsAvailable.toFixed(2)} 积分
                        </td>
                        <td className="p-3">
                          <div>{card.validUntil ? new Date(card.validUntil * 1000).toLocaleDateString() : '未开始 / 未配置'}</div>{card.boundDevices.length} / {card.maxDevices}{' '}
                          {card.boundDevices.length > 0 ? `(${card.boundDevices.join(', ')})` : '(未绑定)'}
                        </td>
                        <td className="p-3">
                          {card.status === 'active' && (
                            <span className="px-2 py-0.5 bg-[#EDF5F0] text-[#39816D] border border-emerald-800 rounded">活跃</span>
                          )}
                          {card.status === 'unactivated' && (
                            <span className="px-2 py-0.5 bg-[#EFF1EF] text-[#23272B] border border-[#E5E8E5] rounded">未激活</span>
                          )}
                          {card.status === 'frozen' && (
                            <span className="px-2 py-0.5 bg-[#FFF5EA] text-[#A87029] border border-amber-800 rounded">已冻结</span>
                          )}
                          {card.status === 'banned' && (
                            <span className="px-2 py-0.5 bg-[#FBEAE5] text-[#B94B39] border border-rose-800 rounded">已封禁</span>
                          )}
                          {card.status === 'expired' && <span>已到期</span>}
                          {card.archivedAt != null && <span className="muted"> · 已归档</span>}
                          {card.status === 'voided' && (
                            <span className="px-2 py-0.5 bg-zinc-800 text-zinc-400 border border-zinc-700 rounded">已删除（作废）</span>
                          )}
                        </td>
                        <td className="p-3 space-x-2">
                          <button disabled={!card.codeRecoverable || revealing || cardBulkBusy} title={card.codeRecoverable ? '按需读取卡密，关闭后清除' : '历史卡密不可恢复'} onClick={async () => {
                            setRevealing(true);
                            try { const result = await adminApi.revealCard(card.id); if (!result.success || !result.rawCode) throw new Error('卡密不可恢复'); setRevealedCard({cardId: card.id, rawCode: result.rawCode}); }
                            catch (err: any) {setActionError(`查看失败：${err.message}`);} finally {setRevealing(false);}
                          }}>{card.codeRecoverable ? '查看卡密' : '无可用明文'}</button>{!card.codeRecoverable && <small className="block muted">此卡未保留明文，请查阅原发放记录。</small>}
                          <button
                            disabled={card.status === 'voided' || cardBulkBusy || loading || !!dataFailures.cards || mutationBusy}
                            title={card.status === 'voided' ? '已删除卡密不可调账，请通过账本查看作废记录' : '调整积分并保留账本记录'}
                            onClick={() => {
                              if(cardBulkBusy || card.status === 'voided')return;
                              if(!operator){setActionError('请先重新登录确认调账账户。');return;}
                              try{
                                const saved=loadAdjustment(sessionStorage,operator);
                                if(saved&&saved.cardId!==card.id){setActionError(`卡 ${saved.cardId} 的调账结果未确认，请先打开原卡核对。`);return;}
                                adjustment.current=saved;setSelectedCard(card);setAdjustAmount(saved?String(saved.delta):'10');setAdjustReason(saved?.reason??'');setShowAdjustModal(true);
                              }catch{setActionError('调账恢复记录无法读取，请人工核对账本；未发送请求。');}
                            }}
                            className="text-[#B94B39] hover:underline"
                          >
                            调账
                          </button>
                          {card.status === 'active' && (
                            <button
                              disabled={cardBulkBusy || loading || !!dataFailures.cards || mutationBusy} onClick={() => handleCardStatus(card.id, 'freeze')}
                              className="text-[#A87029] hover:underline"
                            >
                              冻结
                            </button>
                          )}
                          {card.status === 'frozen' && (
                            <button
                              disabled={cardBulkBusy || loading || !!dataFailures.cards || mutationBusy} onClick={() => handleCardStatus(card.id, 'unfreeze')}
                              className="text-[#39816D] hover:underline"
                            >
                              解冻
                            </button>
                          )}
                          {!['banned', 'voided'].includes(card.status) && (
                            <button
                              disabled={cardBulkBusy || loading || !!dataFailures.cards || mutationBusy} onClick={() => handleCardStatus(card.id, 'ban')}
                              className="danger"
                            >
                              封禁
                            </button>
                          )}
                        </td>
                      </tr>
                    ))
                  ) : (
                    <tr>
                      <td colSpan={7} className="p-6 text-center text-[#7B8388]">
                        <ListEmptyState loading={loading} failed={dataFailures.cards} empty={!cards.length ? '尚无卡密记录。可通过「批量生成」创建卡密。' : '没有符合当前筛选条件的卡密。'} onRetry={() => void refreshData()} />
                        {!loading && !dataFailures.cards && cards.length > 0 && <button disabled={cardBulkBusy} onClick={() => {resetCardFilters(); setCardStatusFilter('ALL');}}>查看全部记录</button>}
                      </td>
                    </tr>
                  )}
                </tbody>
              </table>
                <div className="card-pagination"><button disabled={cardBulkBusy || !!dataFailures.cards || loading || currentCardPage === 0} onClick={() => {setSelectedCardIds([]); setCardPage(currentCardPage - 1);}}>上一页</button><span>第 {currentCardPage + 1} / {pageCount} 页 · 每页 50 条 · 共 {filteredCards.length} 条</span><button disabled={cardBulkBusy || !!dataFailures.cards || loading || currentCardPage + 1 >= pageCount} onClick={() => {setSelectedCardIds([]); setCardPage(currentCardPage + 1);}}>下一页</button></div>
            </div>
          )}

          {activeTab === 'cards' && issuanceRecovery && <section className="notice-panel" aria-label="制卡结果核对">
            <h3>上次制卡结果待核对</h3>{issuanceReference && <p className="card-note">{issuanceReference}</p>}<p>请求可能已经入库，已暂停再次制卡。请刷新卡密列表，按下方批次编号搜索，并核对数量；已生成的卡密可在列表中查看或导出。</p>
            <div className="actions"><button disabled={!issuanceReference} onClick={() => {setSearchQuery(issuanceReference);setGroupFilter('ALL');setCardStatusFilter('ALL');}}>按本批次筛选</button><button disabled={issuanceChecking || loading} onClick={() => void refreshIssuanceForReview()}>{issuanceChecking ? '正在刷新卡密…' : '刷新卡密以核对'}</button><button disabled={issuanceChecking || issuanceRecovery !== 'review'} onClick={() => {if(!window.confirm('确认已核对最新卡密列表？已有本次生成结果时，请勿重复制卡。解除限制不会自动生成。'))return;try{sessionStorage.removeItem(issuanceStorageKey);setIssuanceRecovery(null);setActionError('');}catch{setActionError('无法清除待核对记录，仍禁止制卡。请检查浏览器存储。');}}}>已核对列表，解除制卡限制</button></div>
          </section>}
          {/* TAB 3: GROUPS */}
          {activeTab === 'groups' && isAuthenticated && <CommercialEditor key="groups" kind="groups" onDirtyChange={markCommercialDirty} onBusyChange={markEditorBusy} />}

          {/* TAB 4: PROVIDERS */}
          {activeTab === 'providers' && isAuthenticated && (
            <div className="space-y-4">
              {providerBusy && <p role="status">正在更新供应商状态，请等待结果…</p>}
              <div className="flex justify-between items-center">
                <h3 className="font-semibold text-[#23272B]">供应商连接与 API 密钥</h3>
                <div className="flex gap-2">
                  <button disabled title="独立定时连通性测速尚未配置" className="px-3 py-1.5 bg-[#EFF1EF] text-[#7B8388] rounded text-xs border border-[#E5E8E5] cursor-not-allowed"> 连通性测速 (未配置)</button>
                  <button onClick={() => selectProviderKey()} title="打开供应商与密钥编辑器" className="px-3 py-1.5 bg-[#EFF1EF] text-[#7B8388] rounded text-xs font-medium border border-[#E5E8E5]">管理 API 密钥</button>
                </div>
              </div>
              {providers.length > 0 ? (
                providers.map((p: any) => (
                  <div key={p.id} className="p-4 rounded-xl bg-white border border-[#E5E8E5] space-y-4">
                    <div className="provider-heading flex justify-between items-center border-b border-[#E5E8E5] pb-3">
                      <div>
                        <span className="font-bold text-[#23272B]">{p.name || p.id}</span>
                        <span className="ml-2 text-xs text-[#7B8388] font-mono">{p.base_url || '地址未配置'}</span>
                      </div>
                      <div className="flex items-center gap-3">
                        <span className={`text-xs px-2 py-0.5 rounded font-mono ${p.enabled !== false ? 'bg-[#EDF5F0] text-[#39816D] border border-emerald-800' : 'bg-[#FBEAE5] text-[#B94B39] border border-rose-800'}`}>
                          {p.enabled !== false ? '● 启用中' : '○ 已停用'}
                        </span>
                        <button
                          disabled={providerBusy || loading || !!dataFailures.providers} aria-busy={providerBusy}
                          onClick={() => handleToggleProvider(p.id, p.enabled !== false)}
                          className={`px-2.5 py-1 text-xs rounded border transition-colors ${p.enabled !== false ? 'bg-[#FFF5EA] hover:bg-[#FFF5EA] text-[#A87029] border-amber-800' : 'bg-[#EDF5F0] hover:bg-[#EDF5F0] text-[#39816D] border-emerald-800'}`}
                        >
                          {p.enabled !== false ? '停用此上游' : '启用此上游'}
                        </button>
                      </div>
                    </div>
                    {providerKeys.filter((k: any) => k.provider_id === p.id).length > 0 ? (
                      <div className="overflow-x-auto">
                        <p className="table-hint">左右滑动表格查看状态和编辑操作。</p>
                        <table className="w-full text-left text-xs">
                          <thead className="text-[#7B8388]">
                            <tr>
                              <th className="py-2">密钥 ID</th>
                              <th className="py-2">模型授权</th>
                              <th className="py-2">权重</th>
                              <th className="py-2">状态</th><th className="py-2">操作</th>
                            </tr>
                          </thead>
                          <tbody className="divide-y divide-slate-800 text-[#23272B]">
                            {providerKeys.filter((k: any) => k.provider_id === p.id).map((k: any) => (
                              <tr key={k.id}>
                                <td className="py-2 font-mono text-[#475467]">{k.id}</td>
                                <td className="py-2 font-mono"><span>密钥不返回浏览器</span><div className="text-[#7B8388]">{Array.isArray(k.allowed_models) ? k.allowed_models.join(', ') || '不允许任何模型' : '旧版未限制（建议迁移）'}</div></td>
                                <td className="py-2">{k.weight ?? 1}</td>
                                <td className="py-2"><span className={`status-badge ${k.enabled !== false && k.health_state === 'healthy' ? 'status-success' : ''}`}>{k.enabled === false ? '已停用' : ({healthy: '正常', cooldown: '冷却中', degraded: '异常', unknown: '未检测'}[String(k.health_state)] ?? String(k.health_state || '未检测'))}</span></td><td><button onClick={() => selectProviderKey(k)}>编辑 →</button></td>
                              </tr>
                            ))}
                          </tbody>
                        </table>
                      </div>
                    ) : (
                      <div className="text-xs text-[#7B8388]">该供应商暂无返回的 Key 记录。</div>
                    )}
                  </div>
                ))
              ) : (
                <div className="p-6 rounded-xl bg-white border border-[#E5E8E5] text-[#7B8388] text-xs text-center">
                  <ListEmptyState loading={loading} failed={dataFailures.providers} empty="尚未配置供应商。请使用「添加供应商」配置连接与密钥。" onRetry={() => void refreshData()} />
                </div>
              )}
            </div>
          )}

          {activeTab === 'providers' && isAuthenticated && <ProviderKeyEditor key={`${selectedProviderKey?.provider_id ?? 'new'}:${selectedProviderKey?.id ?? 'new'}`} selectedKey={providerKeys.find(key => key.id === selectedProviderKey?.id && key.provider_id === selectedProviderKey?.provider_id)} onDirtyChange={markProviderDirty} onBusyChange={markEditorBusy} onSaved={saved => {if(saved){setProviderKeys(keys => keys.some(key => key.id === saved.id && key.provider_id === saved.provider_id) ? keys.map(key => key.id === saved.id && key.provider_id === saved.provider_id ? {...key, ...saved} : key) : [...keys, saved]);setSelectedProviderKey(saved);}void refreshData();}} />}

          {/* TAB 5: MODELS */}
          {activeTab === 'models' && isAuthenticated && <CommercialEditor key="models" kind="models" onDirtyChange={markCommercialDirty} onBusyChange={markEditorBusy} />}

          {activeTab === 'traces' && <div className="space-y-6"><section className="panel trace-toolbar" role="search" aria-label="追踪筛选">
            <div className="filter-fields"><label>搜索调用记录<input aria-label="搜索调用记录" placeholder="请求 ID、卡密、模型或状态" value={traceQuery} onChange={e => {setTraceQuery(e.target.value); setTracePage(0);}} /></label>
              <label>请求结果<select aria-label="追踪状态筛选" value={traceStatusFilter} onChange={e => {setTraceStatusFilter(e.target.value); setTracePage(0);}}><option value="ALL">全部结果</option><option value="error">失败</option><option value="client_aborted">客户端中断</option><option value="in_progress">处理中</option><option value="success">成功</option></select></label></div>
            <div className="filter-summary"><p className="muted" role="status">最近 {traces.length} 条记录，匹配 {filteredTraces.length} 条。仅筛选已加载记录。</p>{traceFiltersChanged && <button onClick={resetTraceFilters}>清除追踪筛选</button>}</div>
            <div className="actions"><button disabled={currentTracePage === 0} onClick={() => setTracePage(currentTracePage - 1)}>上一页</button><span>第 {currentTracePage + 1} / {tracePageCount} 页 · 每页 20 条</span><button disabled={currentTracePage + 1 >= tracePageCount} onClick={() => setTracePage(currentTracePage + 1)}>下一页</button></div></section>
            <section className="panel"><table><caption className="sr-only">最近调用追踪，仅包含已加载样本</caption><thead><tr><th>请求 ID</th><th>模型</th><th>首字耗时</th><th>结算</th><th>结果</th><th>操作</th></tr></thead><tbody>{filteredTraces.slice(currentTracePage * 20, (currentTracePage + 1) * 20).map((trace, index) => <tr key={String(trace.id ?? index)} className={selectedTrace === trace ? 'selected-row' : ''}><td>{String(trace.id ?? '—')}</td><td>{String(trace.exposed_model ?? '—')}</td><td>{trace.ttft_ms == null ? '—' : `${trace.ttft_ms} ms`}</td><td>{trace.credits_charged == null ? '—' : `${(Number(trace.credits_charged)/1_000_000).toFixed(6)} 积分`}</td><td><span className={`status-badge status-${String(trace.status)}`}>{traceStatusLabel(trace.status)}</span></td><td><button onClick={() => {setSelectedTrace(trace); document.getElementById('trace-detail')?.focus();}}>详情 →</button></td></tr>)}{!filteredTraces.length && <tr><td colSpan={6} className="empty-state"><ListEmptyState loading={loading} failed={dataFailures.traces} empty={traces.length ? '没有符合条件的请求，请调整或清除筛选。' : '暂无调用记录。这里只显示服务端保留的最近样本。'} onRetry={() => void refreshData()} /></td></tr>}</tbody></table></section>
            <div className="two-columns"><section id="trace-detail" tabIndex={-1} className="panel"><h3>请求详情</h3>{selectedTrace ? <><p>{String(selectedTrace.id)}</p><p className="muted">{new Date(Number(selectedTrace.ts)*1000).toLocaleString()} · 卡密 {String(selectedTrace.card_id)}</p><p>Tokens：{String(selectedTrace.input_tokens ?? '—')} / {String(selectedTrace.output_tokens ?? '—')} · 速率 {String(selectedTrace.tokens_per_second ?? '—')} Tokens/s</p><pre>{JSON.stringify(selectedTrace.attempt_chain ?? [], null, 2)}</pre><button className="primary" onClick={async () => {try {await navigator.clipboard.writeText(String(selectedTrace.id)); showToast('已复制请求 ID');} catch {showToast('复制失败，请手动复制请求 ID');}}}>复制请求 ID</button></> : <p className="muted">选择请求查看重试链路、Tokens 与结算详情。</p>}</section></div>
            <section className="notice-panel"><h3>失败处理</h3><p>临时错误仅在首段输出前按策略重试。发生部分输出后不重放流，避免重复内容与扣费。</p></section>
          </div>}

          {/* TAB 7: RECONCILIATION */}
          {activeTab === 'reconciliation' && (
            <div className="space-y-4">
              <div className="flex justify-between items-center">
                <h3 className="font-semibold text-[#23272B]">财务对账与生命周期管理</h3>
                <div className="flex gap-2">
                  <button onClick={() => handleExportLedger('json')} className="px-3 py-1.5 bg-[#EFF1EF] hover:bg-[#EFF1EF] text-[#23272B] rounded text-xs border border-[#E5E8E5]">导出对账 JSON</button>
                  <button onClick={() => handleExportLedger('csv')} className="px-3 py-1.5 bg-[#B94B39] text-white hover:bg-[#B94B39] text-white rounded text-xs font-medium">导出对账 CSV</button>
                </div>
              </div>
              <section className="panel"><table><thead><tr><th>统计口径</th><th>结算请求</th><th>已扣积分</th><th>收入关联</th><th>状态</th></tr></thead><tbody><tr><td>当前保留账本</td><td>{financials?.dashboard.total_requests ?? '—'}</td><td>{financials ? (financials.dashboard.total_credits_charged / 1_000_000).toLocaleString() : '—'}</td><td>待关联实际充值账本</td><td>{financials ? '已读取 · 尚未人工核对' : '未读取'}</td></tr></tbody></table></section>
              <FinancialPanel data={financials} onPublished={refreshData} onDirtyChange={markCommercialDirty} onBusyChange={markEditorBusy}/>
              <div className="grid grid-cols-2 gap-4">
                  <div className="p-4 rounded-xl bg-white border border-[#E5E8E5] space-y-3">
                    <h4 className="font-bold text-[#23272B] text-sm">模型账本成本记录（非实际利润）</h4>
                    {financials?.modelRankings?.length ? financials.modelRankings.map((row, index) => (
                      <div key={index} className="flex justify-between items-center p-2 rounded bg-[#EFF1EF] text-xs">
                        <span className="text-[#23272B] font-medium">{String(row.model_id ?? '')}</span>
                        <span className="text-[#7B8388]">账本成本记录：{estimatedMoney(row.provider_cost_micro_cny)} · 0 不代表采购免费，覆盖范围见上方</span>
                      </div>
                    )) : <ListEmptyState loading={loading} failed={dataFailures.financials} empty="暂无模型账本成本记录，不能据此判断成本为零。" onRetry={() => void refreshData()} />}
                  </div>
                <div className="p-4 rounded-xl bg-white border border-[#E5E8E5] space-y-3">
                  <h4 className="font-bold text-[#23272B] text-sm">调用追踪保留与清理</h4>
                  <p className="text-xs text-[#7B8388]">手动清理 30 天以前的调用链日志。清理不可撤销，请先完成审计与备份。</p>
                  <div className="flex items-center gap-3 pt-2">
                    <button disabled={pruning} aria-busy={pruning} onClick={() => void handlePruneTraces()} className="danger">{pruning ? '正在清理，请稍候…' : '永久清理 30 天前的追踪'}</button>
                    {pruning && <p role="status">清理正在提交，请勿重复操作。</p>}
                    <span className="text-xs text-[#7B8388]">按服务端接口执行，结果可复核</span>
                  </div>
                </div>
              </div>
            </div>
          )}

          {activeTab === 'announcements' && <div className="space-y-6">
            {noticeRecovery && <section className="notice-panel" aria-label="公告发布结果核对">
              <p role="status">上次公告可能已发布，暂时禁止再次发布。请刷新并核对下方公告的标题、正文和发布时间；已有相同公告时不要重复发布。</p>
              <div className="actions"><button disabled={noticeChecking} onClick={() => void refreshNoticesForReview()}>{noticeChecking ? '正在刷新公告…' : '刷新公告以核对'}</button>
                <button disabled={noticeChecking || noticeRecovery !== 'review'} onClick={() => {if (window.confirm('确认已核对刷新后的公告列表？若已有相同公告，请勿再次发布。解除限制不会自动提交。')) {try{sessionStorage.removeItem(noticeStorageKey);setNoticeRecovery(null);setActionError('');}catch{setActionError('无法清除待核对记录，仍禁止发布。');}}}}>已核对列表，解除发布限制</button></div>
            </section>}
            <section className="panel"><table><thead><tr><th>标题</th><th>范围</th><th>发布时间</th><th>状态</th><th>操作</th></tr></thead><tbody>{announcements.map(notice => <tr key={notice.id}><td>{notice.title}</td><td>全部用户</td><td>{new Date(notice.created_at * 1000).toLocaleString()}</td><td>{!notice.enabled ? '已停用' : notice.expires_at && notice.expires_at < Date.now()/1000 ? '已到期' : '已发布'}</td><td><details><summary>预览</summary><p>{notice.content}</p></details>{notice.enabled && <button disabled={mutationBusy || !isAuthenticated} onClick={() => void handleWithdrawNotice(notice)}>撤回</button>}</td></tr>)}{!announcements.length && <tr><td colSpan={5} className="empty-state"><ListEmptyState loading={loading} failed={dataFailures.announcements} empty="尚无公告。可在下方编辑并预览，确认后发布。" onRetry={() => void refreshData()} /></td></tr>}</tbody></table></section>
            <div className="two-columns"><section className="panel"><h3>编辑公告</h3><div className="field-grid"><label>标题<input aria-label="公告标题" value={noticeTitle} onChange={e => setNoticeTitle(e.target.value)} /></label><label>等级<select aria-label="公告等级" value={noticeLevel} onChange={e => setNoticeLevel(e.target.value as typeof noticeLevel)}><option value="info">普通提示</option><option value="warning">预警通知</option><option value="critical">紧急通知</option></select></label><label className="full-width">正文<textarea rows={4} aria-label="正文内容" value={noticeContent} onChange={e => setNoticeContent(e.target.value)} /></label></div></section><section className="panel"><h3>用户侧预览</h3><h4>{noticeTitle || '尚未填写标题'}</h4><p className="preview-content">{noticeContent || '填写正文后在此预览。'}</p><p className="muted">全部用户 · 发布后有效期 7 天</p><div className="actions"><button className="primary" disabled={!isAuthenticated || !!noticeRecovery || !noticeTitle.trim() || !noticeContent.trim()} onClick={() => setShowNoticeModal(true)}>预览并确认发布</button></div></section></div>
            <section className="notice-panel"><h3>发布确认</h3><p>公告发布后对全部用户可见，有效期为 7 天。请确认正文、通知等级和服务状态准确。</p></section>
          </div>}

          {/* TAB 9: SECURITY */}
          {activeTab === 'security' && (
            <div className="space-y-4">
              <section className="panel"><h3>配置发布审计</h3><p className="muted">记录分组、模型与定价的配置发布。卡密调账记录请在财务对账中查看。</p><table><thead><tr><th>时间</th><th>操作人 / 原因</th><th>原版本</th><th>发布版本</th></tr></thead><tbody>{audit.map((row, index) => <tr key={index}><td>{row.created_at_secs ? new Date(Number(row.created_at_secs) * 1000).toLocaleString() : '—'}</td><td>{String(row.operator ?? '—')} · {String(row.reason ?? '—')}</td><td>{String(row.previous_revision ?? '—')}</td><td>{String(row.revision ?? '—')}</td></tr>)}{!audit.length && <tr><td colSpan={4} className="empty-state"><ListEmptyState loading={loading || auditLoading} failed={!!auditError} empty="暂无配置发布审计记录" onRetry={() => void refreshData()} /></td></tr>}</tbody></table></section>
              <div className="flex justify-between items-center">
                <h3 className="font-semibold text-[#23272B]">部署安全状态</h3>
                <button onClick={() => showToast('KEK 轮换必须通过外部密钥管理/部署流程执行')} className="px-3 py-1.5 bg-[#EFF1EF] hover:bg-[#EFF1EF] text-[#23272B] rounded text-xs font-medium">KEK 轮换说明</button>
              </div>
              <div className="two-columns"><section className="panel"><h3>管理员会话</h3><p>使用管理员账号和密码登录，会话有效期 15 分钟。</p><p>当前会话：{isAuthenticated ? '有效' : '未认证'}</p><div className="actions"><button className="primary" onClick={() => void handleLogout(false)}>退出登录</button><button className="danger" disabled={!isAuthenticated} onClick={() => void handleLogout(true)}>全部会话下线</button></div></section><section className="panel"><h3>双因素验证</h3><p>{adminApi.twoFactorEnabled === true ? '已启用：登录时需输入动态验证码。' : adminApi.twoFactorEnabled === false ? '未启用：当前仅使用账号和密码登录。' : '暂时无法确认双因素验证状态，请重新验证会话。'}</p><p className="muted">双因素验证由服务器配置管理，本页面不支持修改。</p></section></div>
              <div className="grid grid-cols-2 gap-4">
                <div className="p-4 rounded-xl bg-white border border-[#E5E8E5] space-y-3">
                  <h4 className="font-bold text-[#23272B] text-sm">主密钥 (KEK) 保护状态</h4>
                  <div className="text-xs space-y-1 text-[#23272B]">
                    <div>加密算法: <span className="text-[#39816D] font-mono">AES-256-GCM (ring::aead)</span></div>
                    <div>注入源: <span className="text-[#23272B] font-mono">外部环境变量 (KIRO_MASTER_KEK)</span></div>
                    <div>已配置 API 密钥: <span className="font-mono text-[#23272B]">{providersLoaded ? providerKeys.length : '未读取'}</span></div>
                    <div>登录失败保护: <span className="text-[#23272B] font-mono">服务端配置</span></div>
                  </div>
                </div>
                <div className="p-4 rounded-xl bg-white border border-[#E5E8E5] space-y-3">
                  <h4 className="font-bold text-[#23272B] text-sm">登录保护与数据隔离</h4>
                  <div className="text-xs space-y-1 text-[#23272B]">
                    <div>数据隔离: <span className="text-[#23272B] font-mono">当前为单实例账本，不提供多租户隔离</span></div>
                    <div>会话保护: <span className="text-[#39816D] font-mono">短时 HttpOnly Cookie + CSRF</span></div>
                    <div>动态验证码: <span className="text-[#A87029]">{adminApi.twoFactorEnabled===true?'已启用 · 登录强制动态验证码':adminApi.twoFactorEnabled===false?'未启用 · 当前使用用户名和密码登录':'配置状态未确认'}</span></div>
                  </div>
                </div>
              </div>
            </div>
          )}
        </div>
      </main>

      {revealedCard && isAuthenticated && <div role="dialog" tabIndex={-1} aria-modal="true" aria-label="查看卡密" className="fixed inset-0 bg-black/70 flex items-center justify-center z-50">
        <section className="bg-white p-6 rounded-xl space-y-4"><h3>卡密：{revealedCard.cardId}</h3>
          <p role="alert">{actionError}</p><p>仅在需要时查看，请勿分享截图。关闭后清除当前明文。</p>
          <input aria-label="卡密明文" readOnly value={revealedCard.rawCode} onFocus={e => e.target.select()} />
          <div className="actions"><button onClick={async () => {try {await navigator.clipboard.writeText(revealedCard.rawCode); setActionError(''); showToast('已复制卡密');} catch {setActionError('复制失败：浏览器未授权剪贴板，请选中明文手动复制。');}}}>复制卡密</button><button onClick={() => setRevealedCard(null)}>关闭并清除</button></div>
        </section></div>}
      {/* Administrator login */}
      {generatedCards.length > 0 && isAuthenticated && (
        <div role="dialog" tabIndex={-1} aria-modal="true" aria-label="新生成的卡密" className="fixed inset-0 bg-black/70 flex items-center justify-center z-50">
          <div className="bg-white border border-[#E5E8E5] p-6 rounded-xl w-full max-w-2xl max-h-[90vh] overflow-auto space-y-4">
            <p role="alert">{actionError}</p><h3 className="font-bold text-[#23272B]">已生成 {generatedCards.length} 张卡密</h3>
            <p className="text-[#A87029] text-sm">关闭后清除本次生成结果，支持恢复的卡密可在列表中按需查看。下载文件包含敏感凭据，请妥善保管。</p>
            <div className="max-h-64 overflow-auto"><table><thead><tr><th>卡密 ID</th><th>分组</th><th>明文卡密</th><th>操作</th></tr></thead><tbody>{generatedCards.map(card => <tr key={card.cardId}><td>{card.cardId}</td><td>{card.groupId}</td><td className="font-mono">{card.rawCode}</td><td><button onClick={async () => {try {await navigator.clipboard.writeText(card.rawCode); setActionError(''); showToast('已复制该卡密');} catch {setActionError('复制失败：浏览器未授权剪贴板，请选中下方明文或下载 CSV。');}}}>复制此卡</button></td></tr>)}</tbody></table></div>
            <textarea aria-label="一次性卡密结果" readOnly value={generatedCards.map(card => card.rawCode).join('\n')}
              className="w-full h-64 bg-[#EFF1EF] p-3 rounded text-[#23272B] font-mono text-sm" />
            <div className="flex justify-end gap-3 text-sm">
              <button onClick={async () => {
                try { await navigator.clipboard.writeText(generatedCards.map(card => card.rawCode).join('\n')); setActionError(''); showToast('已复制卡密'); }
                catch { setActionError('复制失败：浏览器未授权剪贴板，请使用 CSV 下载。'); }
              }} className="px-3 py-2 bg-[#EFF1EF] rounded">复制卡密</button>
              <button onClick={downloadGeneratedCards} className="px-3 py-2 bg-[#B94B39] text-white rounded">下载 CSV</button>
              <button onClick={clearGeneratedCards} className="px-3 py-2 bg-[#EFF1EF] rounded">确认并清除</button>
            </div>
          </div>
        </div>
      )}

      {/* Batch Generator Modal */}
      {showBatchModal && (
        <div role="dialog" tabIndex={-1} aria-modal="true" aria-label="批量生成卡密" aria-busy={loading} className="fixed inset-0 bg-black/70 flex items-center justify-center z-50">
          <div className="bg-white border border-[#E5E8E5] p-6 rounded-xl w-96 space-y-4">
            {actionError && <p role="alert">{actionError}</p>}
            <h3 className="font-bold text-[#23272B] text-base">批量生成卡密</h3>
            <div className="space-y-3 text-xs">
              <div>
                <label htmlFor="batch-template" className="text-[#7B8388] block mb-1">积分套餐 · 单卡单设备</label>
                <select
                  id="batch-template" disabled={loading} aria-label="积分套餐" value={batchTemplate}
                  onChange={(e) => setBatchTemplate(e.target.value)}
                  className="w-full bg-[#EFF1EF] border border-[#E5E8E5] p-2 rounded text-[#23272B]"
                >
                  {tiers.map(tier => <option key={tier.id} value={tier.id}>{tier.name} · {tier.points.toLocaleString()} 积分 · ¥{tier.price_cny} · 单设备</option>)}
                </select>
              </div>
              <div><label htmlFor="batch-group" className="text-[#7B8388] block mb-1">模型与计费分组</label><select id="batch-group" aria-label="模型与计费分组" className="w-full" value={batchGroup} onChange={e => setBatchGroup(e.target.value)} disabled={loading || !issuableGroups.length}><option value="" disabled>{issuableGroups.length ? '请选择模型与计费分组' : '暂无可发卡分组，无法发卡'}</option>{issuableGroups.map(group => <option key={String(group.id)} value={String(group.id)}>{String(group.name ?? group.id)}</option>)}</select></div>
              <p className="muted">套餐决定积分额度、名称和 30 天有效期；分组决定模型和价格。四档套餐可共用分组，切换套餐不改变分组。</p>
              <p className="muted" aria-label="发卡摘要">{issuanceSummary}</p>
              <div>
                <label htmlFor="batch-count" className="text-[#7B8388] block mb-1">生成数量（1–500 张）</label>
                <input
                  type="number"
                  id="batch-count" disabled={loading} aria-label="生成数量" aria-describedby="batch-count-help" aria-invalid={!validBatchCount} min="1" step="1"
                  max="500"
                  value={batchCount}
                  onChange={(e) => setBatchCount(e.target.value)}
                  className="w-full bg-[#EFF1EF] border border-[#E5E8E5] p-2 rounded text-[#23272B] font-mono"
                />
              </div>
            </div>
            <p id="batch-count-help" className={!validBatchCount ? "field-error" : "muted"}>{!validBatchCount ? '请输入 1–500 的整数，不会自动修改你输入的数量。' : `本次生成 ${batchCountValue} 张；合计 ${((selectedTier?.points ?? 0) * batchCountValue).toLocaleString()} 积分。`}</p>
            <div className="flex justify-end gap-2 pt-2">
              <button disabled={loading} onClick={() => setShowBatchModal(false)} className="px-3 py-1.5 bg-[#EFF1EF] text-[#23272B] rounded text-xs">取消</button>
              <button onClick={handleBatchGenerate} disabled={!!issuanceRecovery || loading || !selectedGroup || !validBatchCount} className="px-3 py-1.5 bg-[#B94B39] text-white hover:bg-[#B94B39] text-white rounded text-xs font-medium disabled:opacity-50">
                {loading ? '正在生成并入库...' : '生成并入库'}
              </button>
            </div>
          </div>
        </div>
      )}

      {/* Adjust Balance Modal */}
      {showAdjustModal && (
        <div role="dialog" tabIndex={-1} aria-modal="true" aria-label="卡密调账" aria-busy={mutationBusy} className="fixed inset-0 bg-black/70 flex items-center justify-center z-50">
          <div className="bg-white border border-[#E5E8E5] p-6 rounded-xl w-96 space-y-4">
            {actionError && <p role="alert">{actionError}</p>}
            <h3 className="font-bold text-[#23272B] text-base">人工调账 · 记录积分增减</h3><p className="muted">关闭弹窗不撤销已发送请求；结果未确认时，重新打开此卡可用原参数重试。</p>
            {adjustment.current&&isZeroMicroAdjustment(adjustment.current)&&<div role="alert"><p>旧意图金额换算为零微积分，无法入账。清除后可重新填写，不会自动提交。</p><button disabled={mutationBusy} onClick={()=>{if(!adjustment.current||!window.confirm('该原意图换算为零微积分，无法入账。确认清除无效意图？'))return;try{clearAdjustment(sessionStorage,adjustment.current);adjustment.current=null;setAdjustAmount('');setActionError('无效意图已清除，请重新填写金额；未发送请求。');}catch{setActionError('本地记录清除失败，请检查浏览器存储。');}}}>清除零微积分意图</button></div>}
            {selectedCard && (
              <div className="text-xs text-[#B94B39] font-mono">
                目标卡密: {selectedCard.id} (上次读取的可用积分: {selectedCard.pointsAvailable.toFixed(2)})
              </div>
            )}
            <div className="space-y-3 text-xs">
              <div>
                <label htmlFor="adjust-amount" className="text-[#7B8388] block mb-1">增减积分数量（负数为扣减）</label>
                <input
                  type="number"
                  step="any"
                  id="adjust-amount" aria-describedby="adjust-help" aria-label="增减积分数量" disabled={mutationBusy||!!adjustment.current} value={adjustAmount}
                  onChange={(e) => setAdjustAmount(e.target.value)}
                  className="w-full bg-[#EFF1EF] border border-[#E5E8E5] p-2 rounded text-[#23272B] font-mono"
                />
              </div>
              <div>
                <label htmlFor="adjust-reason" className="text-[#7B8388] block mb-1">调账原因（写入审计记录）</label>
                <input
                  type="text"
                  placeholder="例: 补偿上游抖动中断 10 积分"
                  id="adjust-reason" maxLength={500} aria-label="调账原因说明" disabled={mutationBusy||!!adjustment.current} value={adjustReason}
                  onChange={(e) => setAdjustReason(e.target.value)}
                  className="w-full bg-[#EFF1EF] border border-[#E5E8E5] p-2 rounded text-[#23272B]"
                />
              </div>
            </div>
            <p id="adjust-help" className="muted">正数增加积分，负数扣减积分；最多六位小数，不能为 0。原因限 1–500 字，请勿填写密码、卡密或令牌。</p>
            {mutationBusy && <p role="status">正在提交调账，请勿关闭页面或重复提交…</p>}
            <div className="flex justify-end gap-2 pt-2">
              <button disabled={mutationBusy} onClick={() => { setShowAdjustModal(false); setSelectedCard(null); }} className="px-3 py-1.5 bg-[#EFF1EF] text-[#23272B] rounded text-xs">取消</button>
              <button disabled={mutationBusy} onClick={handleAdjustSubmit} className="px-3 py-1.5 bg-[#B94B39] text-white rounded text-xs font-medium">确认调账</button>
            </div>
          </div>
        </div>
      )}

      {/* KEK Rotation Modal */}
      {showKekModal && (
        <div role="dialog" tabIndex={-1} aria-modal="true" aria-label="主密钥轮换说明" className="fixed inset-0 bg-black/70 flex items-center justify-center z-50">
          <div className="bg-white border border-[#E5E8E5] p-6 rounded-xl w-96 space-y-4">
            {actionError && <p role="alert">{actionError}</p>}
            <h3 className="font-bold text-[#23272B] text-base">主密钥 (KEK) 轮换向导</h3>
            <p className="text-xs text-[#7B8388]">系统将以旧 KEK 批量解密全部 Provider Key 并以新 KEK 重新 AES-256-GCM 封装写入。</p>
            <div className="space-y-3 text-xs">
              <div>
                <p className="text-[#7B8388]">此页面不接收主密钥。请通过部署层或运维安全 CLI 完成轮换，不要在浏览器中输入明文主密钥。</p>
              </div>
            </div>
            <div className="flex justify-end gap-2 pt-2">
              <button onClick={() => setShowKekModal(false)} className="px-3 py-1.5 bg-[#EFF1EF] text-[#23272B] rounded text-xs">取消</button>
              <button disabled title="主密钥 (KEK) 轮换属于基础设施级操作，需通过部署层或运维安全 CLI 执行，Web 端不支持传输明文主密钥" className="px-3 py-1.5 bg-[#EFF1EF] text-[#7B8388] rounded text-xs font-medium cursor-not-allowed border border-[#E5E8E5]">需经部署层/CLI执行</button>
            </div>
          </div>
        </div>
      )}

      {/* Notice Modal */}
      {showNoticeModal && (
        <div role="dialog" tabIndex={-1} aria-modal="true" aria-label="发布服务公告" aria-busy={mutationBusy} className="fixed inset-0 bg-black/70 flex items-center justify-center z-50">
          <div className="bg-white border border-[#E5E8E5] p-6 rounded-xl w-96 space-y-4">
            {actionError && <p role="alert">{actionError}</p>}
            <h3 className="font-bold text-[#23272B] text-base">发布服务公告</h3>
            <div className="space-y-3 text-xs">
              <div>
                <label htmlFor="notice-title" className="text-[#7B8388] block mb-1">公告标题（最多 256 字）</label>
                <input
                  type="text"
                  placeholder="例: 维护通知"
                  id="notice-title" disabled={mutationBusy} aria-label="公告标题" value={noticeTitle}
                  onChange={(e) => setNoticeTitle(e.target.value)}
                  className="w-full bg-[#EFF1EF] border border-[#E5E8E5] p-2 rounded text-[#23272B]"
                />
              </div>
              <div>
                <label htmlFor="notice-level" className="text-[#7B8388] block mb-1">公告等级</label>
                <select
                  id="notice-level" disabled={mutationBusy} aria-label="公告等级" value={noticeLevel}
                  onChange={(e) => setNoticeLevel(e.target.value as any)}
                  className="w-full bg-[#EFF1EF] border border-[#E5E8E5] p-2 rounded text-[#23272B]"
                >
                  <option value="info">普通提示</option>
                  <option value="warning">预警通知</option>
                  <option value="critical">紧急通知</option>
                </select>
              </div>
              <div>
                <label htmlFor="notice-content" className="text-[#7B8388] block mb-1">正文内容（最多 20,000 字）</label>
                <textarea
                  rows={3}
                  placeholder="输入下发给客户端的公告内容..."
                  id="notice-content" disabled={mutationBusy} aria-label="正文内容" value={noticeContent}
                  onChange={(e) => setNoticeContent(e.target.value)}
                  className="w-full bg-[#EFF1EF] border border-[#E5E8E5] p-2 rounded text-[#23272B]"
                />
              </div>
            </div>
            <p className="muted">面向全部用户，发布后有效期 7 天。请核对维护时间、影响范围和联系方式，勿包含内部凭据。</p>
            {mutationBusy && <p role="status">正在发布公告，请等待服务端确认，勿重复提交…</p>}
            <div className="flex justify-end gap-2 pt-2">
              <button disabled={mutationBusy} onClick={() => setShowNoticeModal(false)} className="px-3 py-1.5 bg-[#EFF1EF] text-[#23272B] rounded text-xs">取消</button>
              <button disabled={mutationBusy || !!noticeRecovery} onClick={handleCreateNotice} className="px-3 py-1.5 bg-[#B94B39] text-white rounded text-xs font-medium">立即发布</button>
            </div>
          </div>
        </div>
      )}
    </div>
  );
}

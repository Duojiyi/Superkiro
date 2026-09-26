// The signed-in console: sidebar, topbar, the current page, and the data they share.
import {useCallback, useEffect, useRef, useState} from 'react';
import {adminApi, AdminApiError, type AdminAnnouncement, type AdminCardItem, type AdminFinancials, type AdminStats, type AdminTrace, type FinancialSettings} from './api';
import CommercialEditor from './CommercialEditor';
import {ConfirmHost, confirmAction} from './components/confirm';
import {IconClose, IconRefresh, IconWarning} from './components/icons';
import {isModalOpen, ModalRootContext} from './components/modal';
import {ToastHost, toast} from './components/toast';
import {TopbarSlotContext} from './components/ui';
import {formatClock, formatFullDateTime, formatSessionLeft} from './format';
import AnnouncementsPage from './pages/Announcements';
import CardsPage from './pages/Cards';
import FinancePage from './pages/Finance';
import OverviewPage from './pages/Overview';
import ProvidersPage, {type KeyEditing} from './pages/Providers';
import SecurityPage from './pages/Security';
import TracesPage from './pages/Traces';
import {publishFailure, type PublishOutcome} from './refusal';
import {brokenRoutes, modelName} from './routes';
import {keyCooldownLeft} from './status';
import type {ErrorAction, Intent, RefreshOptions, Row, Tab} from './types';

const NAV: Array<{group: string; items: Array<{id: Tab; label: string}>}> = [
  {group: '日常', items: [{id: 'overview', label: '运营概览'}, {id: 'cards', label: '卡密资产'}, {id: 'traces', label: '调用追踪'}]},
  {group: '配置', items: [{id: 'groups', label: '分组与权益'}, {id: 'models', label: '模型与定价'}, {id: 'providers', label: '供应商与 Key'}, {id: 'announcements', label: '公告管理'}]},
  {group: '财务与安全', items: [{id: 'reconciliation', label: '财务对账'}, {id: 'security', label: '安全与审计'}]},
];
const TITLES = Object.fromEntries(NAV.flatMap(group => group.items.map(item => [item.id, item.label]))) as Record<Tab, string>;
const WIDE_PAGES: Tab[] = ['cards', 'traces'];

export interface WorkspaceData {
  stats: AdminStats | null;
  cards: AdminCardItem[];
  announcements: AdminAnnouncement[];
  financials: AdminFinancials | null;
  traces: AdminTrace[];
  providers: Row[];
  providerKeys: Row[];
  groups: Row[];
  models: Row[];
  rateCards: Row[];
  settings?: FinancialSettings;
  /** The configuration version the models above belong to: what a publication from outside the editors is checked against. */
  revision?: string;
}

const EMPTY: WorkspaceData = {stats: null, cards: [], announcements: [], financials: null, traces: [], providers: [], providerKeys: [], groups: [], models: [], rateCards: []};

type Section = 'stats' | 'cards' | 'announcements' | 'financials' | 'traces' | 'providers' | 'config';
const SECTION_NAMES: Record<Section, string> = {stats: '统计', cards: '卡密', announcements: '公告', financials: '财务', traces: '调用追踪', providers: '供应商', config: '配置'};
export type Failures = Partial<Record<Section, boolean>>;

function SessionClock({operator}: {operator: string | null}) {
  const [now, setNow] = useState(() => Date.now());
  const expiresAt = adminApi.sessionExpiresAt;
  const left = expiresAt ? expiresAt - now : 0;
  const urgent = left > 0 && left < 130_000;
  useEffect(() => {
    const timer = setInterval(() => setNow(Date.now()), urgent ? 1000 : 15_000);
    return () => clearInterval(timer);
  }, [urgent]);
  return <p className="session-clock" title={expiresAt ? `会话在 ${formatFullDateTime(expiresAt / 1000)} 到期` : undefined}>
    <span className="session-user">{operator ?? '管理员'}</span>
    {expiresAt > 0 && <> · <span className={left < 120_000 ? 'is-warning' : undefined}>剩余 {formatSessionLeft(left)}</span></>}
  </p>;
}

function SessionWarning() {
  const [now, setNow] = useState(() => Date.now());
  useEffect(() => {const timer = setInterval(() => setNow(Date.now()), 1000); return () => clearInterval(timer);}, []);
  const left = adminApi.sessionExpiresAt - now;
  // Only in the last two minutes; a session renewed in another tab makes it go away.
  if (!(left > 0 && left <= 120_000)) return null;
  return <span className="session-warning" role="status">{`会话 ${formatSessionLeft(left)}后到期（草稿会保留）`}</span>;
}

export default function AdminWorkspace({onLogout, operator, expiring, onReauthenticate}: {
  onLogout: (all: boolean) => Promise<void>;
  operator: string | null;
  expiring: boolean;
  onReauthenticate: () => void;
}) {
  const mounted = useRef(true);
  useEffect(() => {mounted.current = true; return () => {mounted.current = false;};}, []);
  // One write at a time across pages: a second action waits until the first is confirmed.
  const writing = useRef(false);
  const guards = {writing, mounted};

  const [activeTab, setActiveTab] = useState<Tab>('overview');
  const [intent, setIntent] = useState<Intent>({});
  const commercialDirty = useRef(false), providerDirty = useRef(false), editorBusy = useRef(false), pageBusy = useRef(false);
  const [busyPage, setBusyPage] = useState(false);
  const markCommercialDirty = useCallback((dirty: boolean) => {commercialDirty.current = dirty;}, []);
  const markProviderDirty = useCallback((dirty: boolean) => {providerDirty.current = dirty;}, []);
  const markEditorBusy = useCallback((busy: boolean) => {editorBusy.current = busy;}, []);
  const markPageBusy = useCallback((busy: boolean) => {pageBusy.current = busy; setBusyPage(busy);}, []);
  useEffect(() => {
    const guard = (event: BeforeUnloadEvent) => {
      if (commercialDirty.current || providerDirty.current || editorBusy.current || pageBusy.current) {event.preventDefault(); event.returnValue = '';}
    };
    window.addEventListener('beforeunload', guard);
    return () => window.removeEventListener('beforeunload', guard);
  }, []);

  const [data, setData] = useState<WorkspaceData>(EMPTY);
  const [loading, setLoading] = useState(true);
  const [failures, setFailures] = useState<Failures>({});
  const [loadErrors, setLoadErrors] = useState<Array<{section: string; message: string; stale: boolean}>>([]);
  const loadedOnce = useRef<Partial<Record<Section, boolean>>>({});
  // A hide (同时隐藏这些模型) whose result was not confirmed: cleared once the configuration is read again.
  const hideUnconfirmed = useRef(false);
  const [syncedAt, setSyncedAt] = useState<number | null>(null);
  const [providersLoaded, setProvidersLoaded] = useState(false);
  // Changes whenever a refresh starts that should clear the card selection.
  const [selectionEpoch, setSelectionEpoch] = useState(0);
  const [actionError, setActionError] = useState<{text: string; action?: ErrorAction} | null>(null);
  const reportError = useCallback((text: string, action?: ErrorAction) => setActionError(text ? {text, action} : null), []);

  const [modalRoot, setModalRoot] = useState<HTMLElement | null>(null);
  const [topbarSlot, setTopbarSlot] = useState<HTMLElement | null>(null);
  const [keyEditing, setKeyEditing] = useState<KeyEditing | null>(null);
  // Bumped by the operator's 刷新, so pages that load on their own (the editors) reload too.
  const [refreshEpoch, setRefreshEpoch] = useState(0);

  // Refreshes can overlap; only the latest may write, so a slow older one never undoes newer data.
  const refreshSeq = useRef(0);
  const refreshData = useCallback(async (options: RefreshOptions = {}) => {
    if (!mounted.current) return;
    const seq = ++refreshSeq.current;
    if (!options.keepSelection) setSelectionEpoch(value => value + 1);
    setLoading(true);
    const errors: Array<{section: Section; message: string}> = [];
    const failed = (section: Section) => (error: unknown) => {
      errors.push({section, message: error instanceof Error ? error.message : String(error)});
      return null;
    };
    try {
      const [stats, cards, notices, financials, traces, providers, config] = await Promise.all([
        adminApi.getStats().catch(failed('stats')),
        adminApi.getCards().catch(failed('cards')),
        adminApi.getAnnouncements().catch(failed('announcements')),
        adminApi.getFinancials().catch(failed('financials')),
        adminApi.getTraces(500).catch(failed('traces')),
        adminApi.getProviders().catch(failed('providers')),
        adminApi.getCommercialConfig().catch(failed('config')),
      ]);
      if (!mounted.current || seq !== refreshSeq.current) return;
      const unavailable: Failures = {
        stats: stats?.success !== true, cards: cards?.success !== true, announcements: notices?.success !== true,
        financials: financials?.success !== true, traces: traces?.success !== true, providers: providers?.success !== true,
        config: config?.success !== true || !config.config,
      };
      // A reply without success is a failure too, never an empty list.
      for (const section of Object.keys(unavailable) as Section[]) {
        if (unavailable[section] && !errors.some(error => error.section === section)) errors.push({section, message: '服务器未确认读取成功'});
      }
      setFailures(unavailable);
      setLoadErrors(errors.map(error => ({section: SECTION_NAMES[error.section], message: error.message, stale: !!loadedOnce.current[error.section]})));
      for (const section of Object.keys(unavailable) as Section[]) if (!unavailable[section]) loadedOnce.current[section] = true;
      if (!errors.length) setSyncedAt(Date.now());
      setData(previous => ({
        stats: stats?.success ? stats : previous.stats,
        cards: cards?.success ? cards.cards : previous.cards,
        announcements: notices?.success ? notices.announcements : previous.announcements,
        financials: financials?.success ? financials : previous.financials,
        traces: traces?.success ? traces.traces : previous.traces,
        providers: providers?.success ? providers.providers ?? [] : previous.providers,
        providerKeys: providers?.success ? providers.keys ?? [] : previous.providerKeys,
        groups: config?.success && config.config ? config.config.groups : previous.groups,
        models: config?.success && config.config ? config.config.models : previous.models,
        rateCards: config?.success && config.config ? config.config.rate_cards : previous.rateCards,
        settings: config?.success && config.config?.settings ? config.config.settings : previous.settings,
        revision: config?.success && config.config ? config.config.revision : previous.revision,
      }));
      if (providers?.success) setProvidersLoaded(true);
      if (config?.success && config.config) hideUnconfirmed.current = false;
    } catch (error) {
      console.error('Failed to load admin data:', error);
      if (mounted.current && seq === refreshSeq.current) setLoadErrors([{section: '全部', message: error instanceof Error ? error.message : String(error), stale: true}]);
    } finally {
      if (mounted.current && seq === refreshSeq.current) setLoading(false);
    }
  }, []);

  useEffect(() => {void refreshData();}, [refreshData]);

  // 同时隐藏这些模型: models hidden on the way to a provider or Key change, published against the
  // configuration version read, with a reason. An unconfirmed result blocks the next one until
  // the configuration has been read again.
  const latestData = useRef(data);
  latestData.current = data;
  const hideModels = useCallback(async (rows: Row[], reason: string): Promise<PublishOutcome> => {
    const {revision, models, groups} = latestData.current;
    if (hideUnconfirmed.current) return {ok: false, message: '上次隐藏模型的结果还没确认：请先点“刷新”核对'};
    if (!revision) return {ok: false, message: '模型配置没有加载，不能隐藏模型：请先点“刷新”'};
    try {
      const result = await adminApi.publishCommercialConfig({models: rows.map(row => ({...row, visible: false})), expected_revision: revision, reason});
      if (result.success !== true) throw new AdminApiError('服务器未确认发布成功', 400);
      if (!result.config?.revision) throw new Error('服务器未返回可核对的配置版本');
      if (mounted.current) setData(previous => ({...previous, models: result.config.models, revision: result.config.revision}));
      return {ok: true};
    } catch (error) {
      const failure = publishFailure(error, '隐藏', id => {const model = models.find(row => row.id === id || row.exposed_model_id === id); return model ? modelName(model, models, groups) : id;});
      if (failure.uncertain) hideUnconfirmed.current = true;
      return failure;
    }
  }, []);

  // Overview and traces refresh themselves every minute, quietly, as background requests (they
  // do not keep the session alive). Paused while anything is open, being written or edited.
  const autoRefresh = activeTab === 'overview' || activeTab === 'traces';
  const loadingRef = useRef(loading);
  loadingRef.current = loading;
  useEffect(() => {
    if (!autoRefresh) return;
    const timer = setInterval(() => {
      if (document.hidden || loadingRef.current || writing.current || pageBusy.current || editorBusy.current || commercialDirty.current || providerDirty.current) return;
      if (isModalOpen() || document.querySelector('.drawer, [role="menu"], [role="alertdialog"]')) return;
      void adminApi.inBackground(() => refreshData({keepSelection: true}));
    }, 60_000);
    return () => clearInterval(timer);
  }, [autoRefresh, refreshData]);

  const navigate = async (tab: Tab, next?: Intent) => {
    if (pageBusy.current) return;
    if (tab === activeTab && !next) return;
    if (editorBusy.current) {toast.info('正在保存，请稍候'); return;}
    if (providerDirty.current && !(await confirmAction({title: '有未保存的修改，确定离开？', consequence: 'Key 的修改还没有保存。', confirmLabel: '离开'}))) return;
    if (commercialDirty.current && !(await confirmAction({title: '有未发布的修改，确定离开？', consequence: '修改还没有发布。', confirmLabel: '离开'}))) return;
    if (!mounted.current || pageBusy.current || editorBusy.current) return;
    commercialDirty.current = false; providerDirty.current = false;
    setIntent(next ?? {});
    setActionError(null);
    setActiveTab(tab);
  };

  const logoutAll = async () => {
    if (!(await confirmAction({title: '下线全部会话？', consequence: '所有管理员（包括你）都要重新登录。', confirmLabel: '下线全部', danger: true}))) return;
    if (mounted.current) await onLogout(true);
  };

  const nowSecs = Date.now() / 1000;
  const failedLastHour = data.traces.filter(trace => trace.status === 'error' && Number(trace.ts) > nowSecs - 3600).length;
  // Keys of a disabled provider serve nothing, so they raise no badge.
  const keyAlerts = data.providerKeys.filter(key => key.enabled !== false && data.providers.find(provider => provider.id === key.provider_id)?.enabled !== false
    && (keyCooldownLeft(key, nowSecs) > 0 || key.health_state === 'degraded')).length;
  // Shown models whose primary route cannot serve (known only once the providers are loaded).
  const broken = providersLoaded ? brokenRoutes(data.models, {providers: data.providers, keys: data.providerKeys}) : [];
  const down = broken.filter(entry => entry.route.down).length;
  const badges: Partial<Record<Tab, {count: number; tone: 'danger' | 'warning'; text: string}>> = {
    ...(failedLastHour ? {traces: {count: failedLastHour, tone: 'danger' as const, text: `近 1 小时 ${failedLastHour} 次失败`}} : {}),
    ...(broken.length ? {models: {count: broken.length, tone: down ? 'danger' as const : 'warning' as const, text: down ? `${down} 个在售模型无可用线路` : `${broken.length} 个在售模型的主线路不可用`}} : {}),
    ...(keyAlerts ? {providers: {count: keyAlerts, tone: 'warning' as const, text: `${keyAlerts} 个 Key 冷却中或异常`}} : {}),
  };
  const staleSections = loadErrors.filter(error => error.stale).map(error => error.section);

  return <ModalRootContext.Provider value={modalRoot}><TopbarSlotContext.Provider value={topbarSlot}>
    <div className="admin-app">
      <a className="skip-link" href="#admin-workspace">跳到工作区</a>
      <aside className="sidebar">
        <h1 className="brand">Superkiro</h1>
        <nav aria-label="管理导航">
          {NAV.map(group => <div key={group.group} className="nav-group">
            <p className="nav-group-label">{group.group}</p>
            {group.items.map(item => {
              const badge = badges[item.id];
              return <button key={item.id} type="button" className="nav-item" aria-label={item.label} aria-current={activeTab === item.id ? 'page' : undefined}
                aria-describedby={badge ? `nav-badge-${item.id}` : undefined} onClick={() => void navigate(item.id)}>
                <span>{item.label}</span>
                {badge && <span id={`nav-badge-${item.id}`} className={`nav-badge nav-badge-${badge.tone}`} title={badge.text}>
                  <span className="sr-only">{badge.text}</span><span aria-hidden="true">{badge.count}</span>
                </span>}
              </button>;
            })}
          </div>)}
        </nav>
        <div className="sidebar-footer">
          <SessionClock operator={operator}/>
          <button type="button" className="btn-text" onClick={() => void onLogout(false)}>退出</button>
        </div>
      </aside>
      <main className="workspace" id="admin-workspace" tabIndex={-1}>
        <header className="topbar">
          <h2 className="page-title">{TITLES[activeTab]}</h2>
          <div className="topbar-actions">
            <div className="topbar-slot" ref={setTopbarSlot}/>
            {expiring && <SessionWarning/>}
            <span className="sync-time" title={syncedAt ? `上次完整加载：${formatFullDateTime(syncedAt / 1000)}` : undefined}>
              {syncedAt ? `${formatClock(syncedAt / 1000)} 更新${autoRefresh ? ' · 自动' : ''}` : '未加载'}
            </span>
            <button type="button" className="btn btn-refresh" disabled={loading || busyPage} onClick={() => {setRefreshEpoch(value => value + 1); void refreshData();}}>
              <IconRefresh/>{loading ? '刷新中…' : '刷新'}
            </button>
          </div>
        </header>
        <div className="page-content">
          <div className={`page-inner${WIDE_PAGES.includes(activeTab) ? ' is-wide' : ''}`}>
            {(loadErrors.length > 0 || actionError) && <div className="page-banners">
              {loadErrors.length > 0 && <div role="alert" className="banner banner-warning">
                <IconWarning/>
                <span className="banner-text">部分数据加载失败：{loadErrors.map(error => error.section).join('、')}{staleSections.length ? `（${staleSections.join('、')}显示的是上次加载的内容）` : ''}</span>
                <button type="button" className="btn btn-small" disabled={loading || busyPage} onClick={() => void refreshData()}>重试</button>
                <details className="banner-details"><summary>详情</summary>
                  <ul>{loadErrors.map((error, index) => <li key={index}>{error.section}：{error.message}</li>)}</ul>
                </details>
              </div>}
              {actionError && <div role="alert" className="banner banner-danger">
                <IconWarning/>
                <span className="banner-text">{actionError.text}</span>
                {actionError.action && <button type="button" className="btn btn-small" onClick={actionError.action.run}>{actionError.action.label}</button>}
                <button type="button" className="btn-icon" aria-label="关闭提示" title="关闭" onClick={() => setActionError(null)}><IconClose/></button>
              </div>}
            </div>}

            {activeTab === 'overview' && <OverviewPage data={data} loading={loading} failures={failures} providersLoaded={providersLoaded}
              operator={operator} onNavigate={(tab, next) => void navigate(tab, next)} onRetry={() => void refreshData()}/>}
            {activeTab === 'cards' && <CardsPage cards={data.cards} groups={data.groups} configFailed={!!failures.config} loading={loading}
              failed={!!failures.cards} operator={operator} refresh={refreshData} guards={guards} reportError={reportError}
              actionError={actionError?.text ?? ''} onBusyChange={markPageBusy} onReauthenticate={onReauthenticate}
              selectionEpoch={selectionEpoch} intent={intent.cards}
              updateCards={cards => setData(previous => ({...previous, cards}))}
              onOpenTrace={(cardId, traceId) => void navigate('traces', {traces: {search: cardId, open: traceId}})}/>}
            {activeTab === 'traces' && <TracesPage traces={data.traces} cards={data.cards} loading={loading} failed={!!failures.traces}
              refresh={refreshData} guards={guards} reportError={reportError} intent={intent.traces}
              onOpenCard={cardId => void navigate('cards', {cards: {status: 'ALL', search: cardId}})}/>}
            {activeTab === 'groups' && <CommercialEditor key="groups" kind="groups" onDirtyChange={markCommercialDirty} onBusyChange={markEditorBusy}
              cards={data.cards} onPublished={() => void refreshData({keepSelection: true})} refreshEpoch={refreshEpoch}/>}
            {activeTab === 'models' && <CommercialEditor key="models" kind="models" onDirtyChange={markCommercialDirty} onBusyChange={markEditorBusy}
              onPublished={() => void refreshData({keepSelection: true})} refreshEpoch={refreshEpoch} providers={data.providers} providerKeys={data.providerKeys}
              routesKnown={providersLoaded} intent={intent.models}/>}
            {activeTab === 'providers' && <ProvidersPage providers={data.providers} providerKeys={data.providerKeys} models={data.models}
              loading={loading} failed={!!failures.providers} refresh={refreshData} guards={guards} reportError={reportError}
              editing={keyEditing} setEditing={setKeyEditing} providerDirty={providerDirty} editorBusy={editorBusy}
              onDirtyChange={markProviderDirty} onBusyChange={markEditorBusy} groups={data.groups} onHideModels={hideModels}
              onListModel={(providerId, model) => void navigate('models', {models: {list: {providerId, model}}})}
              mergeKey={saved => setData(previous => ({...previous, providerKeys: previous.providerKeys.some(key => key.id === saved.id && key.provider_id === saved.provider_id)
                ? previous.providerKeys.map(key => key.id === saved.id && key.provider_id === saved.provider_id ? {...key, ...saved} : key)
                : [...previous.providerKeys, saved]}))}/>}
            {activeTab === 'announcements' && <AnnouncementsPage announcements={data.announcements} loading={loading} failed={!!failures.announcements}
              refresh={refreshData} guards={guards} reportError={reportError}
              updateAnnouncements={announcements => setData(previous => ({...previous, announcements}))}/>}
            {activeTab === 'reconciliation' && <FinancePage financials={data.financials} loading={loading} failed={!!failures.financials}
              refresh={refreshData} reportError={reportError} onDirtyChange={markCommercialDirty} onBusyChange={markEditorBusy} refreshEpoch={refreshEpoch}/>}
            {activeTab === 'security' && <SecurityPage operator={operator} keyCount={providersLoaded ? data.providerKeys.length : null}
              onLogout={() => void onLogout(false)} onLogoutAll={() => void logoutAll()}/>}
          </div>
        </div>
      </main>
      <ToastHost/>
      <div className="modal-root" ref={setModalRoot}/>
      <ConfirmHost/>
    </div>
  </TopbarSlotContext.Provider></ModalRootContext.Provider>;
}

import ProviderKeyEditor from './ProviderKeyEditor';
import CommercialEditor from './CommercialEditor';
import { useState, useEffect, useCallback } from 'react';
import { adminApi, AdminStats, AdminCardItem, AdminAnnouncement, AdminFinancials, GeneratedCard } from './api';

const pages = {overview: ['运营概览', '服务是否稳定，额度是否准确，从这里开始。'], cards: ['卡密资产', '按分组管理权益，所有额度调整保留操作原因。单卡仅限一台设备。'], groups: ['分组与权益', '让套餐权益可读、可比较。单卡单设备，四档积分套餐。'], providers: ['供应商与 Key', '模型能力按 Key 精确授权；发现结果进入草稿，不自动上线。'], models: ['模型与定价', '映射路由、配置积分价格，校验后发布。'], traces: ['调用追踪', '定位失败原因，不记录用户提示词或上游响应正文。'], reconciliation: ['财务对账', '结算积分、估算成本、实际充值分别呈现。'], announcements: ['公告管理', '核对公告正文后发布。当前接口面向全部用户。'], security: ['安全与审计', '高风险操作二次确认，密钥不回显，审计可追溯。']} as const;
const tiers = [{id: 'group-pro', name: 'PRO', points: 1000}, {id: 'group-pro-plus', name: 'PRO+', points: 2000}, {id: 'group-pro-max', name: 'PRO Max', points: 5000}, {id: 'group-power', name: 'Power', points: 10000}];

type Tab = 'overview' | 'cards' | 'groups' | 'providers' | 'models' | 'traces' | 'reconciliation' | 'announcements' | 'security';

export default function App() {
  const [activeTab, setActiveTab] = useState<Tab>('overview');

  // Interactive state
  const [cardStatusFilter, setCardStatusFilter] = useState('ALL');
  const [searchQuery, setSearchQuery] = useState('');
  const [showBatchModal, setShowBatchModal] = useState(false);
  const [generatedCards, setGeneratedCards] = useState<GeneratedCard[]>([]);
  const [showAdjustModal, setShowAdjustModal] = useState(false);
  const [showKekModal, setShowKekModal] = useState(false);
  const [showNoticeModal, setShowNoticeModal] = useState(false);
  const [showKeyModal, setShowKeyModal] = useState(false);
  const [adminKeyInput, setAdminKeyInput] = useState(adminApi.getAdminKey());
  const [isAuthenticated, setIsAuthenticated] = useState<boolean | null>(null);
  const [toastMessage, setToastMessage] = useState<string | null>(null);

  // Live data
  const [stats, setStats] = useState<AdminStats | null>(null);
  const [cards, setCards] = useState<AdminCardItem[]>([]);
  const [announcements, setAnnouncements] = useState<AdminAnnouncement[]>([]);
  const [financials, setFinancials] = useState<AdminFinancials | null>(null);
  const [traces, setTraces] = useState<Array<Record<string, unknown>>>([]);
  const [providers, setProviders] = useState<Array<Record<string, unknown>>>([]);
  const [selectedProviderKey, setSelectedProviderKey] = useState<Record<string, unknown> | undefined>();
  const [providerKeys, setProviderKeys] = useState<Array<Record<string, unknown>>>([]);
  const [loading, setLoading] = useState(false);
  const [syncedAt, setSyncedAt] = useState<string | null>(null);
  const [loadError, setLoadError] = useState('');
  const [groupFilter, setGroupFilter] = useState('ALL');
  const [traceQuery, setTraceQuery] = useState('');

  // Selected card for adjustment
  const [selectedCard, setSelectedCard] = useState<AdminCardItem | null>(null);
  const [adjustAmount, setAdjustAmount] = useState<string>('10');
  const [adjustReason, setAdjustReason] = useState<string>('');

  // Batch generation form
  const [batchCount, setBatchCount] = useState<number>(50);
  const [batchGroup, setBatchGroup] = useState<string>('group-pro-plus');

  // New announcement form
  const [noticeTitle, setNoticeTitle] = useState('');
  const [noticeLevel, setNoticeLevel] = useState<'info' | 'warning' | 'critical'>('info');
  const [noticeContent, setNoticeContent] = useState('');

  const showToast = (msg: string) => {
    setToastMessage(msg);
    setTimeout(() => setToastMessage(null), 3000);
  };

  const refreshData = useCallback(async () => {
    try {
      setLoading(true);
      setLoadError('');
      const authRes = await adminApi.checkAuth().catch(() => ({ success: false }));

      if (authRes && authRes.success) {
        setIsAuthenticated(true);
      } else {
        setIsAuthenticated(false);
        setStats(null); setCards([]); setAnnouncements([]); setFinancials(null); setTraces([]); setProviders([]); setProviderKeys([]); setSyncedAt(null);
        return;
      }

      const [statsRes, cardsRes, noticesRes, financialsRes, tracesRes, providersRes] = await Promise.all([
        adminApi.getStats().catch(() => null),
        adminApi.getCards().catch(() => ({ success: false, count: 0, cards: [] })),
        adminApi.getAnnouncements().catch(() => ({ success: false, announcements: [] })),
        adminApi.getFinancials().catch(() => null),
        adminApi.getTraces(100).catch(() => ({ success: false, traces: [] })),
        adminApi.getProviders().catch(() => ({ success: false, providers: [], keys: [] })),
      ]);

      if (![statsRes, cardsRes, noticesRes, financialsRes, tracesRes, providersRes].every(r => r?.success)) setLoadError('部分数据读取失败，保留最近一次结果。请刷新重试。');
      else setSyncedAt(new Date().toLocaleTimeString([], {hour: '2-digit', minute: '2-digit'}));
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
        setProviders(providersRes.providers || []);
        setProviderKeys(providersRes.keys || []);
      }
    } catch (err) {
      console.error('Failed to load admin data:', err);
    } finally {
      setLoading(false);
    }
  }, []);

  useEffect(() => {
    refreshData();
  }, [refreshData]);

  const handleSaveKey = async () => {
    adminApi.setAdminKey(adminKeyInput.trim());
    try {
      await adminApi.establishSession();
      setShowKeyModal(false);
      setAdminKeyInput('');
      showToast('已建立短时管理会话，正在刷新...');
      await refreshData();
    } catch (err: any) {
      showToast(`认证失败: ${err.message}`);
    }
  };

  const handleLogout = async (all: boolean) => {
    if (all && !window.confirm('确认撤销所有管理员会话？')) return;
    setGeneratedCards([]);
    try {
      await adminApi.logout(all);
      setIsAuthenticated(false);
      showToast(all ? '所有管理会话已全部撤销下线' : '当前管理会话已安全退出');
      await refreshData();
    } catch (err: any) {
      showToast(`退出失败: ${err.message}`);
    }
  };

  const handleCardStatus = async (cardId: string, action: 'freeze' | 'unfreeze' | 'ban') => {
    if (!window.confirm(`确认对卡密 ${cardId} 执行${action === 'freeze' ? '冻结' : action === 'ban' ? '封禁' : '解冻'}？`)) return;
    try {
      const res = await adminApi.updateCardStatus(cardId, action, `管理员手动操作: ${action}`);
      if (res.success) {
        showToast(`卡密 ${cardId} 已${action === 'freeze' ? '冻结' : action === 'unfreeze' ? '解冻' : '封禁'}`);
        refreshData();
      }
    } catch (err: any) {
      showToast(`操作失败: ${err.message}`);
    }
  };

  const handleAdjustSubmit = async () => {
    if (!selectedCard) return;
    const delta = parseFloat(adjustAmount);
    if (!Number.isFinite(delta) || delta === 0) {
      showToast('请输入有效的调账积分数值');
      return;
    }
    if (!adjustReason.trim()) { showToast('请填写调账原因'); return; }
    if (!window.confirm(`确认调整 ${selectedCard.id} 的积分 ${delta > 0 ? '+' : ''}${delta}？`)) return;
    try {
      const res = await adminApi.adjustBalance(selectedCard.id, delta, adjustReason.trim() || '管理员手动调账');
      if (res.success) {
        showToast(`卡密 ${selectedCard.id} 调账成功`);
        setShowAdjustModal(false);
        setSelectedCard(null);
        setAdjustReason('');
        refreshData();
      }
    } catch (err: any) {
      showToast(`调账失败: ${err.message}`);
    }
  };

  const handleBatchGenerate = async () => {
    if (loading || !tiers.some(t => t.id === batchGroup) || !Number.isInteger(batchCount) || batchCount < 1 || batchCount > 500) return;
    if (!window.confirm(`确认生成 ${batchCount} 张 ${tiers.find(t => t.id === batchGroup)?.name} 卡密？每张仅限一台设备，积分与权益以服务端校验为准。`)) return;
    try {
      setLoading(true);
      const res = await adminApi.batchCards(batchCount, batchGroup);
      if (res.success) {
        showToast(`成功批量生成 ${res.cards.length} 张卡密！已持久化入库`);
        setGeneratedCards(res.cards);
        setShowBatchModal(false);
        await refreshData();
      }
    } catch (err: any) {
      showToast(`批量制卡失败: ${err.message}`);
    } finally {
      setLoading(false);
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
    try {
      const res = await adminApi.updateProviderStatus(providerId, !currentEnabled);
      if (res.success) {
        showToast(`供应商 ${providerId} 状态已更新为: ${!currentEnabled ? '已启用' : '已停用'}`);
        await refreshData();
      }
    } catch (err: any) {
      showToast(`切换失败: ${err.message}`);
    }
  };

  const handleExportLedger = async (format: 'json' | 'csv') => {
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
      window.URL.revokeObjectURL(url);
      showToast(`对账 ${format.toUpperCase()} 导出完成！`);
    } catch (err: any) {
      showToast(`导出失败: ${err.message}`);
    }
  };

  const handleCreateNotice = async () => {
    if (!noticeTitle.trim() || !noticeContent.trim()) {
      showToast('公告标题和内容不能为空');
      return;
    }
    if (!window.confirm('确认向全部用户发布该公告？有效期为 7 天。')) return;
    try {
      const res = await adminApi.createAnnouncement(noticeTitle.trim(), noticeContent.trim(), noticeLevel, 86400 * 7);
      if (res.success) {
        showToast('公告已成功发布并同步到网关');
        setShowNoticeModal(false);
        setNoticeTitle('');
        setNoticeContent('');
        refreshData();
      }
    } catch (err: any) {
      showToast(`发布失败: ${err.message}`);
    }
  };

  const filteredCards = cards.filter((c) => {
    if (groupFilter !== 'ALL' && c.groupId !== groupFilter) return false;
    if (cardStatusFilter !== 'ALL' && c.status.toUpperCase() !== cardStatusFilter.toUpperCase()) {
      return false;
    }
    if (searchQuery) {
      const q = searchQuery.toLowerCase();
      return c.id.toLowerCase().includes(q) || (c.note && c.note.toLowerCase().includes(q)) || c.boundDevices.some(d => d.toLowerCase().includes(q));
    }
    return true;
  });

  return (
    <div className="flex h-screen overflow-hidden bg-white text-[#23272B] font-sans text-sm admin-app">
      {/* Toast Notification */}
      {toastMessage && (
        <div className="fixed top-4 right-4 z-50 px-4 py-2 bg-[#B94B39] text-white rounded-lg shadow-xl border border-[#E9C8C1] text-xs ">
          {toastMessage}
        </div>
      )}

      <aside className="sidebar">
        <div><h1>Superkiro</h1><p className="brand-caption">SUPERKIRO / CONTROL</p>
          <nav aria-label="管理导航">{(Object.keys(pages) as Tab[]).map(id => <button key={id} aria-current={activeTab === id ? 'page' : undefined} onClick={() => setActiveTab(id)}>{pages[id][0]}</button>)}</nav>
        </div>
        <div className="sidebar-footer"><p>管理员 · {isAuthenticated ? '会话有效' : '尚未认证'}</p><button onClick={() => setActiveTab('security')}>安全设置</button><span> / </span><button onClick={() => isAuthenticated ? handleLogout(false) : setShowKeyModal(true)}>{isAuthenticated ? '退出' : '登录'}</button></div>
      </aside>
      <main className="workspace">
        <header className="topbar"><span>工作台 / {pages[activeTab][0]}</span><div><span>{syncedAt ? `最近同步 ${syncedAt}` : '尚未同步'}</span><button onClick={() => setShowKeyModal(true)}>{isAuthenticated ? '管理会话' : '管理员登录'}</button><button disabled={loading} onClick={() => refreshData()}>{loading ? '刷新中…' : '刷新'}</button></div></header>
        <div className="page-content">
          <div className="page-heading"><div><h2>{pages[activeTab][0]}</h2><p>{pages[activeTab][1]}</p></div>{activeTab === 'cards' && <button className="primary" onClick={() => setShowBatchModal(true)}>＋ 批量生成</button>}{activeTab === 'providers' && <button className="primary" onClick={() => document.getElementById('key-editor')?.scrollIntoView({behavior: 'smooth'})}>＋ 添加供应商</button>}</div>
          {loadError && <p role="alert" className="notice-panel">{loadError}</p>}
          {activeTab === 'overview' && <div className="space-y-6">
            <div className="metric-grid">
              <section className="panel metric"><p>结算请求</p><strong>{financials?.dashboard.total_requests.toLocaleString() ?? '—'}</strong><small>账本累计口径</small></section>
              <section className="panel metric"><p>卡密资产</p><strong>{stats?.totalCards.toLocaleString() ?? '—'}</strong><small>{stats ? `已激活 ${stats.activeCards} · 未激活 ${stats.unactivatedCards}` : '等待服务端数据'}</small></section>
              <section className="panel metric"><p>已结算积分</p><strong>{financials ? (financials.dashboard.total_credits_charged / 1_000_000).toLocaleString() : '—'}</strong><small>仅含已完成结算</small></section>
              <section className="panel metric"><p>估算上游成本</p><strong>未配置</strong><small>未接入可核验采购口径</small></section>
            </div>
            <div className="overview-grid"><section className="panel"><h3>请求量</h3><p className="muted">最近读取的 {traces.length} 条调用追踪 · 非全天统计</p><div className="request-chart">{traces.length ? Array.from({length: 12}, (_, hour) => { const count = traces.filter(t => { const date = new Date(Number(t.ts) * 1000); return !isNaN(date.getTime()) && Math.floor(date.getHours() / 2) === hour; }).length; return <div className="chart-column" key={hour}><span>{count}</span><div style={{height: `${Math.max(2, count / Math.max(1, traces.length) * 220)}px`}}/><small>{String(hour * 2).padStart(2, '0')}</small></div>; }) : <p className="empty-state">暂无调用追踪数据</p>}</div></section>
            <section className="panel attention"><h3>需要关注</h3><h4>{providerKeys.filter(k => Number(k.cooldown_until ?? 0) > Date.now() / 1000).length} 个 Key 正在冷却</h4><button onClick={() => setActiveTab('providers')}>查看供应商状态 →</button><h4>{stats ? stats.frozenCards + stats.bannedCards : '—'} 张异常卡密</h4><button onClick={() => setActiveTab('cards')}>查看卡密资产 →</button><p className="muted">成本未配置，不展示推测毛利。</p></section></div>
            <section className="panel"><h3>服务健康</h3><table><thead><tr><th>服务</th><th>状态</th><th>Key 数量</th><th>操作</th></tr></thead><tbody>{providers.map(p => <tr key={String(p.id)}><td>{String(p.name || p.id)}</td><td>{p.enabled === false ? '已停用' : '已启用 · 健康状态见 Key'}</td><td>{providerKeys.filter(k => k.provider_id === p.id).length}</td><td><button onClick={() => setActiveTab('providers')}>查看 Key →</button></td></tr>)}{!providers.length && <tr><td colSpan={4} className="empty-state">暂无服务端供应商数据</td></tr>}</tbody></table></section>
          </div>}

          {/* TAB 2: CARDS */}
          {activeTab === 'cards' && (
            <div className="space-y-4">
              <div className="flex justify-between items-center">
                <div className="flex gap-2">
                  <input
                    type="text"
                    aria-label="搜索卡密" placeholder="搜索卡密 ID / 备注 / 设备标识"
                    value={searchQuery}
                    onChange={(e) => setSearchQuery(e.target.value)}
                    className="px-3 py-1.5 bg-white border border-[#E5E8E5] rounded text-xs text-[#23272B] w-64 focus:outline-none focus:border-[#E9C8C1]"
                  />
                  <select aria-label="分组筛选" value={groupFilter} onChange={e => setGroupFilter(e.target.value)}><option value="ALL">全部分组</option>{Array.from(new Set(cards.map(c => c.groupId))).map(id => <option key={id}>{id}</option>)}</select>
                  <select aria-label="状态筛选"
                    value={cardStatusFilter}
                    onChange={(e) => setCardStatusFilter(e.target.value)}
                    className="px-3 py-1.5 bg-white border border-[#E5E8E5] rounded text-xs text-[#23272B]"
                  >
                    <option value="ALL">全部状态</option>
                    <option value="ACTIVE">已激活 (Active)</option>
                    <option value="UNACTIVATED">未激活 (Unactivated)</option>
                    <option value="FROZEN">已冻结 (Frozen)</option>
                    <option value="BANNED">已封禁 (Banned)</option>
                  </select>
                </div>
                <div className="flex gap-2">
                  <button
                    onClick={downloadGeneratedCards}
                    className="px-3 py-1.5 bg-[#EFF1EF] hover:bg-[#EFF1EF] text-[#23272B] rounded text-xs border border-[#E5E8E5]"
                  >
                     导出 CSV
                  </button>
                  <button
                    onClick={() => setShowBatchModal(true)}
                    className="px-3 py-1.5 bg-[#B94B39] text-white hover:bg-[#B94B39] text-white rounded text-xs font-medium"
                  >
                    ➕ 批量制卡 (Generator)
                  </button>
                </div>
              </div>

              <table className="w-full text-left border-collapse bg-white rounded-xl border border-[#E5E8E5] overflow-hidden">
                <thead className="bg-[#EFF1EF] text-xs text-[#7B8388]">
                  <tr>
                    <th className="p-3">卡密 ID</th>
                    <th className="p-3">所属分组</th>
                    <th className="p-3">总积分 / 余额</th>
                    <th className="p-3">到期时间 / 设备绑定</th>
                    <th className="p-3">状态</th>
                    <th className="p-3">操作</th>
                  </tr>
                </thead>
                <tbody className="divide-y divide-slate-800 text-xs text-[#23272B]">
                  {filteredCards.length > 0 ? (
                    filteredCards.map((card) => (
                      <tr key={card.id}>
                        <td className="p-3 font-mono text-[#B94B39]">{card.id}</td>
                        <td className="p-3">{card.groupId}</td>
                        <td className="p-3">
                          {card.pointsTotal.toFixed(2)} / {card.pointsAvailable.toFixed(2)} 积分
                        </td>
                        <td className="p-3">
                          <div>{card.validUntil ? new Date(card.validUntil * 1000).toLocaleDateString() : '未开始 / 未配置'}</div>{card.boundDevices.length} / {card.maxDevices}{' '}
                          {card.boundDevices.length > 0 ? `(${card.boundDevices.join(', ')})` : '(未激活)'}
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
                          {card.status === 'voided' && (
                            <span className="px-2 py-0.5 bg-zinc-800 text-zinc-400 border border-zinc-700 rounded">已作废</span>
                          )}
                        </td>
                        <td className="p-3 space-x-2">
                          <button
                            onClick={() => {
                              setSelectedCard(card);
                              setAdjustAmount('10');
                              setShowAdjustModal(true);
                            }}
                            className="text-[#B94B39] hover:underline"
                          >
                            调账
                          </button>
                          {card.status === 'active' && (
                            <button
                              onClick={() => handleCardStatus(card.id, 'freeze')}
                              className="text-[#A87029] hover:underline"
                            >
                              冻结
                            </button>
                          )}
                          {card.status === 'frozen' && (
                            <button
                              onClick={() => handleCardStatus(card.id, 'unfreeze')}
                              className="text-[#39816D] hover:underline"
                            >
                              解冻
                            </button>
                          )}
                          {card.status !== 'banned' && (
                            <button
                              onClick={() => handleCardStatus(card.id, 'ban')}
                              className="text-[#B94B39] hover:underline"
                            >
                              封禁
                            </button>
                          )}
                        </td>
                      </tr>
                    ))
                  ) : (
                    <tr>
                      <td colSpan={6} className="p-6 text-center text-[#7B8388]">
                        {isAuthenticated ? '暂无匹配的卡密记录' : '请先配置管理密钥 (x-admin-key) 以获取实时卡密数据'}
                      </td>
                    </tr>
                  )}
                </tbody>
              </table>
            </div>
          )}

          {activeTab === 'cards' && <div className="two-columns page-supplement"><section className="panel"><h3>批量生成卡密</h3><p>PRO 1,000 · PRO+ 2,000 · PRO Max 5,000 · Power 10,000</p><p className="muted">每张卡密仅限一台设备。完整卡密只在生成后显示一次，请及时安全交付。</p><div className="actions"><button className="primary" onClick={() => setShowBatchModal(true)}>预览生成清单</button></div></section><section className="notice-panel"><h3>额度调整需要留下原因</h3><p>从卡密记录选择调账，填写增减积分与操作原因，确认后写入账本。</p><p className="muted">当前显示 {filteredCards.length} 条匹配记录，最多读取 500 张卡密。</p></section></div>}
          {/* TAB 3: GROUPS */}
          {activeTab === 'groups' && <CommercialEditor key="groups" kind="groups" />}

          {/* TAB 4: PROVIDERS */}
          {activeTab === 'providers' && (
            <div className="space-y-4">
              <div className="flex justify-between items-center">
                <h3 className="font-semibold text-[#23272B]">上游 Provider 与多 Key 治理</h3>
                <div className="flex gap-2">
                  <button disabled title="独立定时连通性测速尚未配置" className="px-3 py-1.5 bg-[#EFF1EF] text-[#7B8388] rounded text-xs border border-[#E5E8E5] cursor-not-allowed"> 连通性测速 (未配置)</button>
                  <button disabled title="在下方渠道与 Key 编辑器添加供应商" className="px-3 py-1.5 bg-[#EFF1EF] text-[#7B8388] rounded text-xs font-medium cursor-not-allowed border border-[#E5E8E5]">在下方添加渠道与 Key</button>
                </div>
              </div>
              {providers.length > 0 ? (
                providers.map((p: any) => (
                  <div key={p.id} className="p-4 rounded-xl bg-white border border-[#E5E8E5] space-y-4">
                    <div className="flex justify-between items-center border-b border-[#E5E8E5] pb-3">
                      <div>
                        <span className="font-bold text-[#23272B]">{p.name || p.id}</span>
                        <span className="ml-2 text-xs text-[#7B8388] font-mono">{p.base_url || '地址未配置'}</span>
                      </div>
                      <div className="flex items-center gap-3">
                        <span className={`text-xs px-2 py-0.5 rounded font-mono ${p.enabled !== false ? 'bg-[#EDF5F0] text-[#39816D] border border-emerald-800' : 'bg-[#FBEAE5] text-[#B94B39] border border-rose-800'}`}>
                          {p.enabled !== false ? '● 启用中' : '○ 已停用'}
                        </span>
                        <button
                          onClick={() => handleToggleProvider(p.id, p.enabled !== false)}
                          className={`px-2.5 py-1 text-xs rounded border transition-colors ${p.enabled !== false ? 'bg-[#FFF5EA] hover:bg-[#FFF5EA] text-[#A87029] border-amber-800' : 'bg-[#EDF5F0] hover:bg-[#EDF5F0] text-[#39816D] border-emerald-800'}`}
                        >
                          {p.enabled !== false ? '停用此上游' : '启用此上游'}
                        </button>
                      </div>
                    </div>
                    {providerKeys.filter((k: any) => k.provider_id === p.id).length > 0 ? (
                      <div className="overflow-x-auto">
                        <table className="w-full text-left text-xs">
                          <thead className="text-[#7B8388]">
                            <tr>
                              <th className="py-2">Key ID</th>
                              <th className="py-2">脱敏密匙</th>
                              <th className="py-2">权重</th>
                              <th className="py-2">状态</th><th className="py-2">操作</th>
                            </tr>
                          </thead>
                          <tbody className="divide-y divide-slate-800 text-[#23272B]">
                            {providerKeys.filter((k: any) => k.provider_id === p.id).map((k: any) => (
                              <tr key={k.id}>
                                <td className="py-2 font-mono text-[#B94B39]">{k.id}</td>
                                <td className="py-2 font-mono"><span>密钥不返回浏览器</span><div className="text-[#7B8388]">{Array.isArray(k.allowed_models) ? k.allowed_models.join(', ') || '不允许任何模型' : '旧版未限制（建议迁移）'}</div></td>
                                <td className="py-2">{k.weight ?? 1}</td>
                                <td className="py-2"><span className="px-1.5 py-0.5 bg-[#EDF5F0] text-[#39816D] rounded">{k.enabled === false ? "Disabled" : k.health_state || "unknown"}</span></td><td><button onClick={() => {setSelectedProviderKey(k); document.getElementById('key-editor')?.scrollIntoView({behavior: 'smooth'});}}>编辑 →</button></td>
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
                  暂无已读取的供应商记录，请先认证并刷新。
                </div>
              )}
            </div>
          )}

          {activeTab === 'providers' && <ProviderKeyEditor selectedKey={selectedProviderKey} onSaved={() => void refreshData()} />}

          {/* TAB 5: MODELS */}
          {activeTab === 'models' && <CommercialEditor key="models" kind="models" />}

          {/* TAB 6: TRACES */}
          {activeTab === 'traces' && (
            <div className="space-y-4">
              <div className="flex justify-between items-center">
                <input aria-label="筛选请求" placeholder="搜索请求 ID / 卡密 / 模型 / 状态" value={traceQuery} onChange={e => setTraceQuery(e.target.value)} />
                <span className="text-xs text-[#7B8388]">来自服务端持久化 trace，不含对话正文</span>
              </div>
              {traces.length === 0 ? (
                <div className="p-6 rounded-xl bg-white border border-[#E5E8E5] text-[#7B8388] text-xs">当前没有服务端 trace 数据。</div>
              ) : <table className="w-full text-left text-xs bg-white rounded-xl border border-[#E5E8E5] overflow-hidden">
                <thead className="bg-[#EFF1EF] text-[#7B8388]">
                  <tr>
                    <th className="p-3">时间</th>
                    <th className="p-3">卡密</th>
                    <th className="p-3">模型</th>
                    <th className="p-3">状态</th>
                    <th className="p-3">首字 (TTFT)</th>
                    <th className="p-3">速率</th>
                    <th className="p-3">Tokens (入/出)</th>
                    <th className="p-3">扣除积分</th>
                    <th className="p-3">重试链路</th>
                  </tr>
                </thead>
                <tbody className="divide-y divide-slate-800 text-[#23272B]">
                  {traces.filter(trace => !traceQuery || [trace.id, trace.card_id, trace.exposed_model, trace.status].some(v => String(v ?? '').toLowerCase().includes(traceQuery.toLowerCase()))).map((trace, index) => <tr key={String(trace.id ?? index)}>
                    <td className="p-3 text-[#7B8388]">{String(trace.ts ?? '')}</td>
                    <td className="p-3 font-mono text-[#B94B39]">{String(trace.card_id ?? '')}</td>
                    <td className="p-3">{String(trace.exposed_model ?? '')}</td>
                    <td className="p-3">{String(trace.status ?? '')}</td>
                    <td className="p-3">{String(trace.ttft_ms ?? '-')} ms</td>
                    <td className="p-3">{String(trace.tokens_per_second ?? '-')}</td>
                    <td className="p-3 font-mono">{String(trace.input_tokens ?? 0)} / {String(trace.output_tokens ?? 0)}</td>
                    <td className="p-3 font-mono text-[#39816D]">{(Number(trace.credits_charged ?? 0) / 1_000_000).toFixed(6)}</td>
                    <td className="p-3">{Array.isArray(trace.attempt_chain) && trace.attempt_chain.length > 0 ? <details><summary>{trace.attempt_chain.length} 次（首段尝试）</summary><pre className="max-w-xs overflow-auto whitespace-pre-wrap">{JSON.stringify(trace.attempt_chain, null, 2)}</pre></details> : '-'}</td>
                  </tr>)}
                </tbody>
              </table>}
            </div>
          )}

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
              <section className="panel"><table><thead><tr><th>统计口径</th><th>结算请求</th><th>已扣积分</th><th>收入关联</th><th>状态</th></tr></thead><tbody><tr><td>账本累计</td><td>{financials?.dashboard.total_requests ?? '—'}</td><td>{financials ? (financials.dashboard.total_credits_charged / 1_000_000).toLocaleString() : '—'}</td><td>待关联实际充值账本</td><td>{financials ? '已读取 · 尚未人工核对' : '未读取'}</td></tr></tbody></table></section>
              <div className="two-columns"><section className="panel"><h3>成本配置</h3><p className="muted">采购汇率、上游采购价格未接入可核验配置。</p><p>积分价格与实际采购成本必须分别管理。</p></section><section className="panel"><h3>收入与利润</h3><p className="muted">实际到账收入：待关联充值账本</p><p className="muted">上游成本：未配置</p><p>毛利：暂不计算</p></section></div>
              <div className="grid grid-cols-2 gap-4">
                  <div className="p-4 rounded-xl bg-white border border-[#E5E8E5] space-y-3">
                    <h4 className="font-bold text-[#23272B] text-sm">模型成本与利润排行</h4>
                    {financials?.modelRankings?.length ? financials.modelRankings.map((row, index) => (
                      <div key={index} className="flex justify-between items-center p-2 rounded bg-[#EFF1EF] text-xs">
                        <span className="text-[#23272B] font-medium">{String(row.model_id ?? '')}</span>
                        <span className="text-[#7B8388]">账本成本记录：{row.provider_cost_micro_cny == null ? '未提供' : `${(Number(row.provider_cost_micro_cny) / 1_000_000).toFixed(6)} 元`} · 非实际到账毛利</span>
                      </div>
                    )) : <p className="text-xs text-[#7B8388]">暂无服务端财务数据。</p>}
                  </div>
                <div className="p-4 rounded-xl bg-white border border-[#E5E8E5] space-y-3">
                  <h4 className="font-bold text-[#23272B] text-sm">数据保留策略 (Data Retention & Pruning)</h4>
                  <p className="text-xs text-[#7B8388]">手动清理 30 天以前的调用链日志。清理不可撤销，请先完成审计与备份。</p>
                  <div className="flex items-center gap-3 pt-2">
                    <button onClick={async () => { if (!window.confirm('确认永久清理 30 天以前的调用追踪？')) return; try { const result = await adminApi.pruneTraces(Math.floor(Date.now() / 1000) - 30 * 86400); showToast(`已清理 ${result.pruned} 条 trace`); await refreshData(); } catch (err: any) { showToast(`清理失败: ${err.message}`); } }} className="px-3 py-1.5 bg-[#FBEAE5] hover:bg-[#FBEAE5] text-[#B94B39] border border-rose-800 rounded text-xs font-medium">
                      执行就地安全清理 (Prune &gt;30d)
                    </button>
                    <span className="text-xs text-[#7B8388]">按服务端接口执行，结果可复核</span>
                  </div>
                </div>
              </div>
            </div>
          )}

          {activeTab === 'announcements' && <div className="space-y-6">
            <section className="panel"><table><thead><tr><th>标题</th><th>范围</th><th>发布时间</th><th>状态</th><th>操作</th></tr></thead><tbody>{announcements.map(notice => <tr key={notice.id}><td>{notice.title}</td><td>全部用户</td><td>{new Date(notice.created_at * 1000).toLocaleString()}</td><td>{!notice.enabled ? '已停用' : notice.expires_at && notice.expires_at < Date.now()/1000 ? '已到期' : '已发布'}</td><td><details><summary>预览</summary><p>{notice.content}</p></details></td></tr>)}{!announcements.length && <tr><td colSpan={5} className="empty-state">暂无已读取的公告</td></tr>}</tbody></table></section>
            <div className="two-columns"><section className="panel"><h3>编辑公告</h3><div className="field-grid"><label>标题<input value={noticeTitle} onChange={e => setNoticeTitle(e.target.value)} /></label><label>等级<select value={noticeLevel} onChange={e => setNoticeLevel(e.target.value as typeof noticeLevel)}><option value="info">普通提示</option><option value="warning">预警通知</option><option value="critical">紧急通知</option></select></label><label className="full-width">正文<textarea rows={4} value={noticeContent} onChange={e => setNoticeContent(e.target.value)} /></label></div></section><section className="panel"><h3>用户侧预览</h3><h4>{noticeTitle || '尚未填写标题'}</h4><p className="preview-content">{noticeContent || '填写正文后在此预览。'}</p><p className="muted">全部用户 · 发布后有效期 7 天</p><div className="actions"><button className="primary" disabled={!noticeTitle.trim() || !noticeContent.trim()} onClick={() => setShowNoticeModal(true)}>预览并确认发布</button></div></section></div>
            <section className="notice-panel"><h3>发布确认</h3><p>核对受众和正文后发布。不将尚未生效的配置变更描述为已上线。</p></section>
          </div>}

          {/* TAB 9: SECURITY */}
          {activeTab === 'security' && (
            <div className="space-y-4">
              <div className="flex justify-between items-center">
                <h3 className="font-semibold text-[#23272B]">部署安全状态</h3>
                <button onClick={() => showToast('KEK 轮换必须通过外部密钥管理/部署流程执行')} className="px-3 py-1.5 bg-[#EFF1EF] hover:bg-[#EFF1EF] text-[#23272B] rounded text-xs font-medium">KEK 轮换说明</button>
              </div>
              <div className="two-columns"><section className="panel"><h3>管理员会话</h3><p>凭管理员密钥建立短期会话。</p><p>当前会话：{isAuthenticated ? '有效' : '未认证'}</p><div className="actions"><button className="primary" onClick={() => setShowKeyModal(true)}>验证身份</button><button disabled={!isAuthenticated} onClick={() => handleLogout(true)}>全部会话下线</button></div></section><section className="panel"><h3>双因素验证</h3><p>TOTP 尚未接入，需外部身份系统。</p><p className="muted">部署级 RLS 和审计存储需单独验收，不以界面状态代替服务端检查。</p></section></div>
              <div className="grid grid-cols-2 gap-4">
                <div className="p-4 rounded-xl bg-white border border-[#E5E8E5] space-y-3">
                  <h4 className="font-bold text-[#23272B] text-sm">主密钥 (KEK) 保护状态</h4>
                  <div className="text-xs space-y-1 text-[#23272B]">
                    <div>加密算法: <span className="text-[#39816D] font-mono">AES-256-GCM (ring::aead)</span></div>
                    <div>注入源: <span className="text-[#23272B] font-mono">外部环境变量 (KIRO_MASTER_KEK)</span></div>
                    <div>Provider Key 数: <span className="font-mono text-[#23272B]">由服务端实时返回</span></div>
                    <div>登录防爆破机制: <span className="text-[#23272B] font-mono">服务端配置</span></div>
                  </div>
                </div>
                <div className="p-4 rounded-xl bg-white border border-[#E5E8E5] space-y-3">
                  <h4 className="font-bold text-[#23272B] text-sm">管理员 2FA 与多租户 RLS</h4>
                  <div className="text-xs space-y-1 text-[#23272B]">
                    <div>数据隔离: <span className="text-[#23272B] font-mono">当前单进程账本；部署级 RLS 未接入</span></div>
                    <div>管理认证: <span className="text-[#39816D] font-mono">短时 Bearer Session</span></div>
                    <div>TOTP 2FA: <span className="text-[#A87029]">未实现，需外部身份系统</span></div>
                  </div>
                </div>
              </div>
            </div>
          )}
        </div>
      </main>

      {/* Admin Key Modal */}
      {showKeyModal && (
        <div className="fixed inset-0 bg-black/70 flex items-center justify-center z-50">
          <div className="bg-white border border-[#E5E8E5] p-6 rounded-xl w-96 space-y-4">
            <h3 className="font-bold text-[#23272B] text-base">管理员密钥配置 (Admin Key)</h3>
            <p className="text-xs text-[#7B8388]">
              请输入网关环境变量 <code className="text-[#B94B39]">ADMIN_KEY</code> 或 <code className="text-[#B94B39]">ADMIN_SECRET</code>。
            </p>
            <div>
              <input
                type="password"
                placeholder="输入管理员密钥 (x-admin-key)..."
                value={adminKeyInput}
                onChange={(e) => setAdminKeyInput(e.target.value)}
                className="w-full bg-[#EFF1EF] border border-[#E5E8E5] p-2 rounded text-[#23272B] font-mono text-xs focus:outline-none focus:border-[#E9C8C1]"
              />
            </div>
            <div className="flex justify-end gap-2 pt-2">
              <button onClick={() => setShowKeyModal(false)} className="px-3 py-1.5 bg-[#EFF1EF] text-[#23272B] rounded text-xs">取消</button>
              <button onClick={handleSaveKey} className="px-3 py-1.5 bg-[#B94B39] text-white rounded text-xs font-medium">保存并验证</button>
            </div>
          </div>
        </div>
      )}

      {generatedCards.length > 0 && (
        <div role="dialog" aria-modal="true" aria-label="新生成的卡密" className="fixed inset-0 bg-black/70 flex items-center justify-center z-50">
          <div className="bg-white border border-[#E5E8E5] p-6 rounded-xl w-full max-w-2xl space-y-4">
            <h3 className="font-bold text-[#23272B]">已生成 {generatedCards.length} 张卡密</h3>
            <p className="text-[#A87029] text-sm">卡密仅在此次生成后显示，关闭或刷新页面后无法再次取回。下载文件包含敏感凭据，请妥善保管。</p>
            <textarea aria-label="一次性卡密结果" readOnly value={generatedCards.map(card => card.rawCode).join('\n')}
              className="w-full h-64 bg-[#EFF1EF] p-3 rounded text-[#23272B] font-mono text-sm" />
            <div className="flex justify-end gap-3 text-sm">
              <button onClick={async () => {
                try { await navigator.clipboard.writeText(generatedCards.map(card => card.rawCode).join('\n')); showToast('已复制卡密'); }
                catch { showToast('复制失败，请使用 CSV 下载'); }
              }} className="px-3 py-2 bg-[#EFF1EF] rounded">复制卡密</button>
              <button onClick={downloadGeneratedCards} className="px-3 py-2 bg-[#B94B39] text-white rounded">下载 CSV</button>
              <button onClick={clearGeneratedCards} className="px-3 py-2 bg-[#EFF1EF] rounded">确认并清除</button>
            </div>
          </div>
        </div>
      )}

      {/* Batch Generator Modal */}
      {showBatchModal && (
        <div className="fixed inset-0 bg-black/70 flex items-center justify-center z-50">
          <div className="bg-white border border-[#E5E8E5] p-6 rounded-xl w-96 space-y-4">
            <h3 className="font-bold text-[#23272B] text-base">批量生成卡密</h3>
            <div className="space-y-3 text-xs">
              <div>
                <label className="text-[#7B8388] block mb-1">积分套餐 · 单卡单设备</label>
                <select
                  value={batchGroup}
                  onChange={(e) => setBatchGroup(e.target.value)}
                  className="w-full bg-[#EFF1EF] border border-[#E5E8E5] p-2 rounded text-[#23272B]"
                >
                  {tiers.map(tier => <option key={tier.id} value={tier.id}>{tier.name} · {tier.points.toLocaleString()} 积分 · 单设备</option>)}
                </select>
              </div>
              <div>
                <label className="text-[#7B8388] block mb-1">生成数量 (1 ~ 500)</label>
                <input
                  type="number"
                  min="1"
                  max="500"
                  value={batchCount}
                  onChange={(e) => setBatchCount(Math.max(1, Math.min(500, parseInt(e.target.value) || 1)))}
                  className="w-full bg-[#EFF1EF] border border-[#E5E8E5] p-2 rounded text-[#23272B] font-mono"
                />
              </div>
            </div>
            <div className="flex justify-end gap-2 pt-2">
              <button onClick={() => setShowBatchModal(false)} className="px-3 py-1.5 bg-[#EFF1EF] text-[#23272B] rounded text-xs">取消</button>
              <button onClick={handleBatchGenerate} disabled={loading} className="px-3 py-1.5 bg-[#B94B39] text-white hover:bg-[#B94B39] text-white rounded text-xs font-medium disabled:opacity-50">
                {loading ? '正在生成并入库...' : '生成并入库'}
              </button>
            </div>
          </div>
        </div>
      )}

      {/* Adjust Balance Modal */}
      {showAdjustModal && (
        <div className="fixed inset-0 bg-black/70 flex items-center justify-center z-50">
          <div className="bg-white border border-[#E5E8E5] p-6 rounded-xl w-96 space-y-4">
            <h3 className="font-bold text-[#23272B] text-base">人工调账 (写入账本条目)</h3>
            {selectedCard && (
              <div className="text-xs text-[#B94B39] font-mono">
                目标卡密: {selectedCard.id} (当前积分: {selectedCard.pointsAvailable.toFixed(2)})
              </div>
            )}
            <div className="space-y-3 text-xs">
              <div>
                <label className="text-[#7B8388] block mb-1">增减积分数量 (负数为扣减)</label>
                <input
                  type="number"
                  step="any"
                  value={adjustAmount}
                  onChange={(e) => setAdjustAmount(e.target.value)}
                  className="w-full bg-[#EFF1EF] border border-[#E5E8E5] p-2 rounded text-[#23272B] font-mono"
                />
              </div>
              <div>
                <label className="text-[#7B8388] block mb-1">调账原因说明 (写入 reason 留痕)</label>
                <input
                  type="text"
                  placeholder="例: 补偿上游抖动中断 10 积分"
                  value={adjustReason}
                  onChange={(e) => setAdjustReason(e.target.value)}
                  className="w-full bg-[#EFF1EF] border border-[#E5E8E5] p-2 rounded text-[#23272B]"
                />
              </div>
            </div>
            <div className="flex justify-end gap-2 pt-2">
              <button onClick={() => { setShowAdjustModal(false); setSelectedCard(null); }} className="px-3 py-1.5 bg-[#EFF1EF] text-[#23272B] rounded text-xs">取消</button>
              <button onClick={handleAdjustSubmit} className="px-3 py-1.5 bg-[#B94B39] text-white rounded text-xs font-medium">确认调账</button>
            </div>
          </div>
        </div>
      )}

      {/* KEK Rotation Modal */}
      {showKekModal && (
        <div className="fixed inset-0 bg-black/70 flex items-center justify-center z-50">
          <div className="bg-white border border-[#E5E8E5] p-6 rounded-xl w-96 space-y-4">
            <h3 className="font-bold text-[#23272B] text-base">主密钥 (KEK) 轮换向导</h3>
            <p className="text-xs text-[#7B8388]">系统将以旧 KEK 批量解密全部 Provider Key 并以新 KEK 重新 AES-256-GCM 封装写入。</p>
            <div className="space-y-3 text-xs">
              <div>
                <label className="text-[#7B8388] block mb-1">新主密钥 (64位 Hex 或留空自动生成)</label>
                <input type="password" placeholder="留空由 CSPRNG 自动生成..." className="w-full bg-[#EFF1EF] border border-[#E5E8E5] p-2 rounded text-[#23272B] font-mono" />
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
        <div className="fixed inset-0 bg-black/70 flex items-center justify-center z-50">
          <div className="bg-white border border-[#E5E8E5] p-6 rounded-xl w-96 space-y-4">
            <h3 className="font-bold text-[#23272B] text-base">发布服务公告</h3>
            <div className="space-y-3 text-xs">
              <div>
                <label className="text-[#7B8388] block mb-1">公告标题</label>
                <input
                  type="text"
                  placeholder="例: 维护通知"
                  value={noticeTitle}
                  onChange={(e) => setNoticeTitle(e.target.value)}
                  className="w-full bg-[#EFF1EF] border border-[#E5E8E5] p-2 rounded text-[#23272B]"
                />
              </div>
              <div>
                <label className="text-[#7B8388] block mb-1">公告等级</label>
                <select
                  value={noticeLevel}
                  onChange={(e) => setNoticeLevel(e.target.value as any)}
                  className="w-full bg-[#EFF1EF] border border-[#E5E8E5] p-2 rounded text-[#23272B]"
                >
                  <option value="info">Info (普通提示)</option>
                  <option value="warning">Warning (预警通知)</option>
                  <option value="critical">Critical (紧急降级)</option>
                </select>
              </div>
              <div>
                <label className="text-[#7B8388] block mb-1">正文内容</label>
                <textarea
                  rows={3}
                  placeholder="输入下发给客户端的公告内容..."
                  value={noticeContent}
                  onChange={(e) => setNoticeContent(e.target.value)}
                  className="w-full bg-[#EFF1EF] border border-[#E5E8E5] p-2 rounded text-[#23272B]"
                />
              </div>
            </div>
            <div className="flex justify-end gap-2 pt-2">
              <button onClick={() => setShowNoticeModal(false)} className="px-3 py-1.5 bg-[#EFF1EF] text-[#23272B] rounded text-xs">取消</button>
              <button onClick={handleCreateNotice} className="px-3 py-1.5 bg-[#B94B39] text-white rounded text-xs font-medium">立即发布</button>
            </div>
          </div>
        </div>
      )}
    </div>
  );
}

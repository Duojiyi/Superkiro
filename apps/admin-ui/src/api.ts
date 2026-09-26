export class AdminApiError extends Error {
  constructor(message:string, public readonly status:number){super(message);this.name='AdminApiError';}
}
/**
 * Kiro BYOK Admin Console Frontend API Contract & Client.
 *
 * Implements Spec §7, P0-01, P0-02, P1-02.
 * Strictly aligned with Gateway Backend /api/v1/admin/* endpoints.
 */

export interface AdminStats {
  success: boolean;
  totalCards: number;
  activeCards: number;
  unactivatedCards: number;
  frozenCards: number;
  bannedCards: number;
  totalCredits: number;
  usedCredits: number;
  remainingCredits: number;
  totalPoints: number;
  usedPoints: number;
  remainingPoints: number;
  /** Real totals over the last 24 hours and 7 days (newer servers). */
  activity?: AdminActivity;
}

export interface AdminActivityWindow {
  requests: number;
  succeeded: number;
  failed: number;
  clientAborted: number;
  /** Micro-credits: divide by 1,000,000 for credits. */
  creditsCharged: number;
  inputTokens: number;
  outputTokens: number;
  providerCostMicroCny: number;
  /** Cards billed at least once in the period. */
  activeCards: number;
  /** Requests that recorded a time to first output, and its median and 90th percentile. */
  timedRequests?: number;
  ttftMedianMs?: number | null;
  ttftP90Ms?: number | null;
}

export interface AdminActivity {
  last24h: AdminActivityWindow;
  last7d: AdminActivityWindow;
  /** The last 24 clock hours, oldest first; the last is the current hour. */
  hourly: Array<{startSecs: number; requests: number; failed: number}>;
  /** The oldest trace kept: request counts reach back no further than this. */
  tracesCoverFromSecs?: number | null;
  /** The last 24 hours by provider, busiest first. */
  providers?: Array<{providerId: string; requests: number; failed: number; ttftMedianMs: number | null}>;
}

export interface AdminTraceAttempt {
  key_id?: string;
  provider_id?: string;
  success?: boolean;
  error?: string | null;
  latency_ms?: number;
}

export interface AdminTrace {
  id: string;
  card_id?: string;
  ts: number;
  invocation_id?: string;
  exposed_model?: string;
  status?: string;
  ttft_ms?: number | null;
  tokens_per_second?: number | null;
  error_class?: string | null;
  provider_id?: string | null;
  input_tokens?: number;
  output_tokens?: number;
  credits_charged?: number;
  provider_cost_micro_cny?: number;
  attempt_chain?: AdminTraceAttempt[];
}

export interface TraceReply {
  status: string;
  error: string | null;
  providerId: string;
  targetModel: string;
  stopReason: string | null;
  text: string;
  reasoning: string;
  toolCalls: Array<{id: string; name: string; arguments: string}>;
  inputTokens: number;
  outputTokens: number;
  cacheReadTokens: number;
  cacheWriteTokens: number;
  ttftMs: number | null;
  tokensPerSecond: number | null;
  truncated: boolean;
}

/** One request as it arrived and the model's reply, kept for 24 hours. */
/** One entry of a card's history. `credits` is micro-credits; for `issued`, the credits issued. */
export interface CardEvent {
  ts: number;
  action: string;
  credits: number;
  points: number;
  operator: string | null;
  reason: string | null;
}

export interface TraceContent {
  success: boolean;
  invocationId: string;
  cardId: string;
  model: string;
  receivedAt: number;
  expiresAt: number;
  request: unknown;
  notes?: {omittedImages?: number; omittedHistoryEntries?: number; truncated?: boolean; unparsed?: boolean};
  reply: TraceReply | null;
}

export interface AdminSessionResponse {
  success: boolean;
  expiresIn: number;
  expiresAt?: number;
  twoFactorEnabled?: boolean;
  totpRequired?: boolean;
  username?: string;
}

export interface FinancialSettings {credit_face_value_cny:number;usd_cny_rate:number;rate_updated_at_secs:number}
export interface AdminFinancials {
  basis?: string;
  settings?: FinancialSettings;
  actualRevenueMicroCny?: number|null;
  actualGrossProfitMicroCny?: number|null;
  estimates?: {usageFaceValueMicroCny:number;configuredProviderCostMicroCny:number;faceValueLessCostMicroCny:number|null;faceValueMarginPercentage:number|null;costedRequests:number;uncostedRequests:number;retainedLedgerOnly:boolean};
  success: boolean;
  dashboard: {
    total_requests: number;
    total_credits_charged: number;
    revenue_micro_cny: number;
    provider_cost_micro_cny: number;
    gross_profit_micro_cny: number;
    gross_margin_percentage: number;
  };
  modelRankings: Array<{model_id?: string; requests?: number; total_tokens?: number; provider_cost_micro_cny?: number; credits_charged?: number; margin_percentage?: number}>;
}

export interface AdminCardItem {
  id: string;
  codeRecoverable: boolean;
  status: 'active' | 'unactivated' | 'frozen' | 'banned' | 'voided' | 'expired';
  creditTotal: number;
  creditUsed: number;
  availableCredits: number;
  pointsTotal: number;
  pointsAvailable: number;
  boundDevices: string[];
  maxDevices: number;
  activatedAt?: number;
  archivedAt?: number | null;
  validUntil?: number;
  groupId: string;
  note?: string;
}

export interface GeneratedCard {
  cardId: string;
  rawCode: string;
  groupId: string;
  creditTotal: number;
  status: string;
}

export interface AdminCardsResponse {
  success: boolean;
  count: number;
  cards: AdminCardItem[];
  offset?: number;
  limit?: number;
  revision?: string;
}

export interface AdminCardStatusResponse {
  success: boolean;
  cardId: string;
  newStatus: string;
}

export interface AdminCardAdjustResponse {
  success: boolean;
  cardId: string;
  newAvailableCredits: number;
  newAvailablePoints: number;
}

/** What one test request to an upstream model found. */
export interface ProbeResult {
  success: boolean;
  ok: boolean;
  /** The upstream's HTTP status (0 when no request was made). */
  status: number;
  latency_ms: number;
  ttft_ms: number | null;
  error: string | null;
  reply: string | null;
}

export interface AdminAnnouncement {
  id: string;
  title: string;
  content: string;
  level: 'info' | 'warning' | 'critical';
  enabled: boolean;
  created_at: number;
  expires_at?: number;
}

// How long before the end of a session the operator is warned.
export const SESSION_WARNING_MS = 120_000;

export class AdminApiClient {
  private baseUrl: string;
  private csrfToken = '';
  // Identity is established only by a successful explicit login, never browser storage.
  authenticatedUsername: string|null = null;
  private sessionVersion = 0;
  private requests = new Set<AbortController>();
  // 'expired': the session reached its fixed lifetime; the login page says so.
  onUnauthorized?: (reason?: 'expired') => void;
  onSessionChanged?: () => void;
  // Shortly before the session ends, so the operator can finish what they are doing.
  onExpiring?: () => void;
  twoFactorEnabled: boolean | undefined;
  totpRequired = false;
  private expiryTimer?: ReturnType<typeof setTimeout>;
  private warningTimer?: ReturnType<typeof setTimeout>;
  private expiresAt = 0;
  // The server's clock minus this one (ms), learned when the session is checked.
  private serverOffset = 0;
  // Requests started inside `inBackground` are automatic: they do not count as use of the session.
  private backgroundCalls = 0;

  /** Starts requests that the operator did not ask for (automatic refresh). */
  inBackground<T>(start: () => Promise<T>): Promise<T> {
    this.backgroundCalls++;
    try {return start();} finally {this.backgroundCalls--;}
  }

  constructor(baseUrl: string = '') { this.baseUrl = baseUrl; }

  async establishSession(username: string, password: string, totpCode?: string): Promise<AdminSessionResponse> {
    this.clearSession();
    const session = await this.request<AdminSessionResponse>('/api/v1/admin/session', {
      method: 'POST', body: JSON.stringify({ username, password, totpCode }),
    });
    if (session.success !== true) throw new Error('登录失败，请重试');
    await this.checkAuth();
    // The server names the operator; older servers do not, and then the name typed here counts.
    if (!this.authenticatedUsername) this.authenticatedUsername = username;
    return session;
  }

  clearSession() {
    clearTimeout(this.expiryTimer);clearTimeout(this.warningTimer);this.expiresAt=0;
    this.authenticatedUsername=null;this.csrfToken = ''; this.sessionVersion++;
    for (const controller of this.requests) controller.abort();
    this.requests.clear();
  }

  async logout(all: boolean = false): Promise<void> {
    const headers = new Headers({'Content-Type': 'application/json', 'x-csrf-token': this.csrfToken});
    this.clearSession();
    const controller=new AbortController();const timer=setTimeout(()=>controller.abort(),15000);
    try {const res = await fetch(`${this.baseUrl}/api/v1/admin/session/revoke`, {
      signal:controller.signal,
      method: 'POST', headers, body: JSON.stringify({all}), credentials: 'same-origin', cache: 'no-store',
    });
    if (!res.ok && res.status !== 401) throw new Error('会话撤销失败');
    }finally{clearTimeout(timer);}
  }

  /**
   * The session ends after 30 idle minutes, so its deadline moves with every use. Each reply
   * names the current deadline (server clock); timers follow it, a little debounced.
   */
  private followDeadline(deadlineSecs: number, serverNowMs?: number) {
    const serverNow = serverNowMs ?? Date.now() + this.serverOffset;
    const remaining = deadlineSecs * 1000 - serverNow;
    if (!Number.isFinite(remaining) || remaining <= 0) return;
    if (this.expiresAt && Math.abs(Date.now() + remaining - this.expiresAt) < 1000) return;
    this.schedule(remaining);
  }

  private schedule(remaining: number) {
    clearTimeout(this.expiryTimer);clearTimeout(this.warningTimer);
    this.expiresAt=Date.now()+remaining;
    this.expiryTimer=setTimeout(()=>this.deadlineReached(),Math.min(2147483647,remaining));
    if(remaining>SESSION_WARNING_MS)this.warningTimer=setTimeout(()=>this.onExpiring?.(),Math.min(2147483647,remaining-SESSION_WARNING_MS));
    else this.onExpiring?.();
  }

  /** Remaining session time from a session reply (its lifetime, not its clock), or null. */
  private remainingFrom(result: {expiresAt?: number; expiresIn?: number}): number | null {
    if(result.expiresAt===undefined&&result.expiresIn===undefined)return null;
    if(typeof result.expiresAt==='number'&&typeof result.expiresIn==='number')this.serverOffset=(result.expiresAt-result.expiresIn)*1000-Date.now();
    // The server's remaining lifetime, not its clock: an operator's clock ahead of the
    // server's by more than the lifetime otherwise ended every new session at once.
    return typeof result.expiresIn==='number'?result.expiresIn*1000:Number(result.expiresAt)*1000-Date.now();
  }

  /**
   * The local deadline passed. Use elsewhere (another tab) may have moved it, so ask the
   * server once, as a background request that does not itself count as use, before ending.
   */
  private deadlineCheck?: Promise<void>;
  private deadlineReached(): Promise<void> {
    // One check at a time, however many requests are waiting on it.
    this.deadlineCheck ??= this.checkDeadline().finally(()=>{this.deadlineCheck=undefined;});
    return this.deadlineCheck;
  }

  private async checkDeadline(): Promise<void> {
    const version=this.sessionVersion;
    if(!this.csrfToken)return;
    try {
      const result=await this.request<SessionCheck>('/api/v1/admin/session',{headers:{'x-admin-background':'1'}},false,true);
      if(version!==this.sessionVersion)return;
      const remaining=result.success===true&&result.role==='admin'&&result.csrfToken===this.csrfToken?this.remainingFrom(result):null;
      if(remaining!==null&&Number.isFinite(remaining)&&remaining>0){this.schedule(remaining);return;}
    } catch {/* Unconfirmed: the session is treated as ended. */}
    if(version!==this.sessionVersion)return;
    this.clearSession();this.onUnauthorized?.('expired');
  }

  private async request<T>(path: string, options: RequestInit = {}, blob = false, internal = false): Promise<T> {
    const background = this.backgroundCalls > 0;
    // Past the deadline this tab knows: confirm with the server before sending anything.
    if(!internal&&this.expiresAt&&Date.now()>=this.expiresAt){await this.deadlineReached();if(!this.csrfToken)throw new Error('请先登录');}
    if (path !== '/api/v1/admin/session' && !this.csrfToken) throw new Error('请先登录');
    const headers = new Headers(options.headers || {});
    if (options.method && !['GET', 'HEAD', 'OPTIONS'].includes(options.method)) headers.set('x-csrf-token', this.csrfToken);
    if (!headers.has('Accept')) headers.set('Accept', 'application/json');
    if (background) headers.set('x-admin-background', '1');
    if (options.body !== undefined) headers.set('Content-Type', 'application/json');
    const version = this.sessionVersion;
    const controller = new AbortController();
    this.requests.add(controller);
    // A download (the whole ledger) needs longer than an ordinary call.
    const timer=setTimeout(()=>controller.abort(),blob?120000:15000);
    try {
      const res = await fetch(`${this.baseUrl}${path}`, {
        ...options, headers, signal: controller.signal, credentials: 'same-origin', cache: 'no-store',
      });
      if (version !== this.sessionVersion) throw new Error('管理会话已改变，请重新加载');
      if (res.status !== 401 && this.csrfToken) {
        const deadline = Number(res.headers?.get?.('x-admin-session-expires'));
        const serverNow = Date.parse(res.headers?.get?.('date') ?? '');
        if (Number.isFinite(deadline) && deadline > 0) this.followDeadline(deadline, Number.isFinite(serverNow) ? serverNow : undefined);
      }
      if (!res.ok) {
        // The deadline check reports its own ending, with the reason.
        if (res.status === 401 && !internal && !(path === '/api/v1/admin/session' && options.method === 'POST')) {
          this.clearSession(); this.onUnauthorized?.();
        }
        const errorVersion=this.sessionVersion;
        const error = await res.json().catch(() => ({}));
        if (errorVersion !== this.sessionVersion) throw new Error('管理会话已改变，请重新加载');
        if(path==='/api/v1/admin/session' && errorVersion===this.sessionVersion){this.twoFactorEnabled=typeof error.twoFactorEnabled==='boolean'?error.twoFactorEnabled:undefined;this.totpRequired=error.totpRequired===true;}
        // Admin errors come as {error} or, from the shared handler, {__type, message}.
        throw new AdminApiError(error.error || error.message || (res.status === 401 ? '用户名或密码错误' : `请求失败 (${res.status})`),res.status);
      }
      const result = await (blob ? res.blob() : res.json());
      if (version !== this.sessionVersion) throw new Error('管理会话已改变，请重新加载');
      return result;
    } catch(error){if(controller.signal.aborted&&version===this.sessionVersion)throw new Error('请求超时，结果未确认；写操作请核对后重试');throw error;} finally {clearTimeout(timer);this.requests.delete(controller);}
  }

  async checkAuth(): Promise<SessionCheck> {
    const version = this.sessionVersion;
    const result = await this.request<SessionCheck>('/api/v1/admin/session');
    if (version !== this.sessionVersion) throw new Error('管理会话已改变，请重新加载');
    if (result.success !== true || result.role !== 'admin' || typeof result.csrfToken !== 'string' || !result.csrfToken) {
      this.clearSession(); this.onUnauthorized?.();
      throw new Error('管理会话无效');
    }
    const changed = !!this.csrfToken && this.csrfToken !== result.csrfToken;
    // A cookie changed in another tab: old requests and sensitive UI belong to the old session.
    if (changed) this.clearSession();
    if(this.csrfToken!==result.csrfToken)this.authenticatedUsername=null;
    this.csrfToken = result.csrfToken;
    // The operator this session belongs to, when the server says (it replaces a re-login to name them).
    if (typeof result.username === 'string' && result.username.trim() && result.username.length <= 128) this.authenticatedUsername = result.username;
    this.twoFactorEnabled=result.twoFactorEnabled;this.totpRequired=result.totpRequired===true;
    clearTimeout(this.expiryTimer);clearTimeout(this.warningTimer);
    const remaining=this.remainingFrom(result);
    if(remaining!==null){
      if(!Number.isFinite(remaining)||remaining<=0){this.clearSession();this.onUnauthorized?.('expired');throw new Error('管理会话已到期');}
      this.schedule(remaining);
    }
    if (changed) this.onSessionChanged?.();
    return result;
  }

  /** When the current session ends (milliseconds since the epoch), or 0 when not known. */
  get sessionExpiresAt(): number { return this.expiresAt; }
  /** Now on the server's clock, as far as the session replies tell (milliseconds). */
  get serverNowMs(): number { return Date.now() + this.serverOffset; }

  async revealCard(cardId: string): Promise<{ success: boolean; rawCode: string }> {
    return this.request('/api/v1/admin/cards/reveal', {method: 'POST', body: JSON.stringify({cardId})});
  }

  async getCommercialConfig(): Promise<{success: boolean; config: CommercialConfig}> {
    return this.request('/api/v1/admin/commercial-config');
  }
  async publishCommercialConfig(update: Record<string, unknown>): Promise<{success: boolean; config: CommercialConfig}> {
    return this.request('/api/v1/admin/commercial-config', {method: 'POST', body: JSON.stringify(update)});
  }

  async getStats(): Promise<AdminStats> {
    return this.request('/api/v1/admin/stats');
  }

  async getCards(): Promise<AdminCardsResponse> {
    const cards: AdminCardItem[] = [], seen = new Set<string>();
    const version = this.sessionVersion;
    let revision: string | undefined;
    for (let offset = 0; ; offset += 500) {
      const page = await this.request<AdminCardsResponse>(`/api/v1/admin/cards?offset=${offset}&limit=500`);
      if (version !== this.sessionVersion) throw new Error('管理会话已改变，请重新加载');
      if (page.success !== true || !Array.isArray(page.cards)) throw new Error('卡密列表读取失败');
      if (!page.revision) throw new Error('服务端未提供卡密分页版本，请升级服务端后刷新核对');
      if (offset === 0) revision = page.revision;
      else if (page.revision !== revision) throw new Error('卡密列表在读取期间发生变化，请重新刷新后核对；本次不使用不完整列表');
      for (const card of page.cards) {
        if (!card.id || seen.has(card.id)) throw new Error('卡密分页包含重复或无效记录，请刷新后重试');
        seen.add(card.id); cards.push(card);
      }
      if (page.cards.length < 500) return {...page, cards, count: cards.length};
    }
  }

  async updateCardStatus(
    cardId: string,
    action: 'freeze' | 'unfreeze' | 'ban' | 'void' | 'archive' | 'unarchive',
    reason?: string
  ): Promise<AdminCardStatusResponse> {
    return this.request('/api/v1/admin/cards/status', {
      method: 'POST',
      body: JSON.stringify({ cardId, action, reason }),
    });
  }

  async adjustBalance(
    cardId: string,
    deltaPoints: number,
    reason: string,
    idempotencyKey: string
  ): Promise<AdminCardAdjustResponse> {
    return this.request('/api/v1/admin/cards/adjust', {
      method: 'POST',
      body: JSON.stringify({ cardId, deltaPoints, reason, idempotencyKey }),
    });
  }

  async getAnnouncements(): Promise<{
    success: boolean;
    announcements: AdminAnnouncement[];
  }> {
    return this.request('/api/v1/admin/announcements');
  }

  async createAnnouncement(
    title: string,
    content: string,
    level: 'info' | 'warning' | 'critical' = 'info',
    ttlSecs?: number
  ): Promise<{ success: boolean; announcement: AdminAnnouncement }> {
    return this.request('/api/v1/admin/announcements', {
      method: 'POST',
      body: JSON.stringify({ title, content, level, ttlSecs }),
    });
  }

  async withdrawAnnouncement(id: string): Promise<{ success: boolean; id: string }> {
    return this.request('/api/v1/admin/announcements/withdraw', {method: 'POST', body: JSON.stringify({id})});
  }

  async getFinancials(): Promise<AdminFinancials> {
    return this.request('/api/v1/admin/financials');
  }

  /** The latest traces, newest first; `cardId` narrows them to one card on the server. */
  async getTraces(limit = 500, cardId?: string): Promise<{ success: boolean; traces: AdminTrace[] }> {
    const count = Math.min(500, Math.max(1, Math.floor(limit)));
    return this.request(`/api/v1/admin/traces?limit=${count}${cardId ? `&card_id=${encodeURIComponent(cardId)}` : ''}`);
  }

  /** One request's content and reply. Every read is logged by the server, naming the operator. */
  async getTraceContent(invocationId: string): Promise<TraceContent> {
    return this.request(`/api/v1/admin/traces/content?invocation_id=${encodeURIComponent(invocationId)}`);
  }

  /** What happened to one card, newest first: who did it and why. */
  async getCardHistory(cardId: string): Promise<{success: boolean; cardId: string; events: CardEvent[]}> {
    return this.request(`/api/v1/admin/cards/history?card_id=${encodeURIComponent(cardId)}`);
  }

  async pruneTraces(cutoffSecs: number): Promise<{ success: boolean; pruned: number }> {
    return this.request('/api/v1/admin/traces/prune', {
      method: 'POST',
      body: JSON.stringify({ cutoffSecs }),
    });
  }

  async batchCards(count: number, groupId: string, templateId = 'tier-2000', note?: string): Promise<{ success: boolean; cards: GeneratedCard[] }> {
    if (!groupId?.trim()) throw new Error('请选择模型与计费分组');
    return this.request('/api/v1/admin/cards/batch', {
      method: 'POST',
      body: JSON.stringify({ count, groupId, templateId, maxDevices: 1, ...(note ? {note} : {}) }),
    });
  }

  async exportLedger(format: 'json' | 'csv'): Promise<Blob> {
    return this.request<Blob>(`/api/v1/admin/exports/ledger.${format}`, {
      headers: { Accept: format === 'csv' ? 'text/csv' : 'application/json' },
    }, true);
  }

  async importProvider(content: Record<string, unknown>): Promise<{success: boolean}> {
    return this.request('/api/v1/admin/providers/import', {method: 'POST', body: JSON.stringify({format: 'cc_switch', content})});
  }

  async manageKey(action: 'save' | 'discover', payload: Record<string, unknown>): Promise<{success: boolean; models?: string[]; has_more?: boolean}> {
    return this.request('/api/v1/admin/providers/keys' + (action === 'discover' ? '/discover' : ''), {method: 'POST', body: JSON.stringify(payload)});
  }

  /** 测试: one tiny real request to this upstream model (a fraction of a cent). Nothing is saved. */
  async probeKey(payload: {provider_id: string; model: string; key_id?: string}): Promise<ProbeResult> {
    return this.request('/api/v1/admin/providers/keys/probe', {method: 'POST', body: JSON.stringify(payload)});
  }

  /** Clears a Key's cooldown and unhealthy state: it takes requests again at once. */
  async resetKey(providerId: string, keyId: string): Promise<{success: boolean}> {
    return this.request('/api/v1/admin/providers/keys/reset', {method: 'POST', body: JSON.stringify({provider_id: providerId, key_id: keyId})});
  }

  /** Refused (409) while it is the only route of a model customers see. */
  async deleteKey(providerId: string, keyId: string): Promise<{success: boolean}> {
    return this.request('/api/v1/admin/providers/keys/delete', {method: 'POST', body: JSON.stringify({provider_id: providerId, key_id: keyId})});
  }

  async getProviders(): Promise<{ success: boolean; providers: Array<Record<string, unknown>>; keys: Array<Record<string, unknown>> }> {
    return this.request('/api/v1/admin/providers');
  }

  async updateProviderStatus(providerId: string, enabled: boolean): Promise<{ success: boolean; providerId: string; enabled: boolean }> {
    return this.request('/api/v1/admin/providers/status', {
      method: 'POST',
      body: JSON.stringify({ providerId, enabled }),
    });
  }
}

export const adminApi = new AdminApiClient();

interface SessionCheck {
  success: boolean;
  role: string;
  csrfToken: string;
  expiresAt?: number;
  expiresIn?: number;
  twoFactorEnabled?: boolean;
  totpRequired?: boolean;
  username?: string;
}

export interface CommercialConfig {
  settings?: FinancialSettings;
  revision: string;
  groups: Array<Record<string, unknown>>;
  models: Array<Record<string, unknown>>;
  rate_cards: Array<Record<string, unknown>>;
  versions: Array<Record<string, unknown>>;
  audit: Array<Record<string, unknown>>;
}

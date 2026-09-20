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
}

export interface AdminSessionResponse {
  success: boolean;
  expiresIn: number;
  expiresAt?: number;
  twoFactorEnabled?: boolean;
  totpRequired?: boolean;
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
  modelRankings: Array<Record<string, unknown>>;
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

export interface AdminAnnouncement {
  id: string;
  title: string;
  content: string;
  level: 'info' | 'warning' | 'critical';
  enabled: boolean;
  created_at: number;
  expires_at?: number;
}

export class AdminApiClient {
  private baseUrl: string;
  private csrfToken = '';
  // Identity is established only by a successful explicit login, never browser storage.
  authenticatedUsername: string|null = null;
  private sessionVersion = 0;
  private requests = new Set<AbortController>();
  onUnauthorized?: () => void;
  twoFactorEnabled: boolean | undefined;
  totpRequired = false;
  private expiryTimer?: ReturnType<typeof setTimeout>;
  private expiresAt = 0;

  constructor(baseUrl: string = '') { this.baseUrl = baseUrl; }

  async establishSession(username: string, password: string, totpCode?: string): Promise<AdminSessionResponse> {
    this.clearSession();
    const session = await this.request<AdminSessionResponse>('/api/v1/admin/session', {
      method: 'POST', body: JSON.stringify({ username, password, totpCode }),
    });
    if (session.success !== true) throw new Error('登录失败，请重试');
    await this.checkAuth();
    this.authenticatedUsername=username;
    return session;
  }

  clearSession() {
    clearTimeout(this.expiryTimer);this.expiresAt=0;
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

  private async request<T>(path: string, options: RequestInit = {}, blob = false): Promise<T> {
    if(this.expiresAt && Date.now()>=this.expiresAt){this.clearSession();this.onUnauthorized?.();}
    if (path !== '/api/v1/admin/session' && !this.csrfToken) throw new Error('请先登录');
    const headers = new Headers(options.headers || {});
    if (options.method && !['GET', 'HEAD', 'OPTIONS'].includes(options.method)) headers.set('x-csrf-token', this.csrfToken);
    if (!headers.has('Accept')) headers.set('Accept', 'application/json');
    if (options.body !== undefined) headers.set('Content-Type', 'application/json');
    const version = this.sessionVersion;
    const controller = new AbortController();
    this.requests.add(controller);
    const timer=setTimeout(()=>controller.abort(),15000);
    try {
      const res = await fetch(`${this.baseUrl}${path}`, {
        ...options, headers, signal: controller.signal, credentials: 'same-origin', cache: 'no-store',
      });
      if (version !== this.sessionVersion) throw new Error('管理会话已改变，请重新加载');
      if (!res.ok) {
        if (res.status === 401 && !(path === '/api/v1/admin/session' && options.method === 'POST')) {
          this.clearSession(); this.onUnauthorized?.();
        }
        const errorVersion=this.sessionVersion;
        const error = await res.json().catch(() => ({}));
        if(path==='/api/v1/admin/session' && errorVersion===this.sessionVersion){this.twoFactorEnabled=typeof error.twoFactorEnabled==='boolean'?error.twoFactorEnabled:undefined;this.totpRequired=error.totpRequired===true;}
        throw new AdminApiError(error.error || (res.status === 401 ? '用户名或密码错误' : `请求失败 (${res.status})`),res.status);
      }
      const result = await (blob ? res.blob() : res.json());
      if (version !== this.sessionVersion) throw new Error('管理会话已改变，请重新加载');
      return result;
    } catch(error){if(controller.signal.aborted&&version===this.sessionVersion)throw new Error('请求超时，结果未确认；写操作请核对后重试');throw error;} finally {clearTimeout(timer);this.requests.delete(controller);}
  }

  async checkAuth(): Promise<{ success: boolean; role: string; csrfToken: string; expiresAt?: number; twoFactorEnabled?: boolean; totpRequired?: boolean }> {
    const version = this.sessionVersion;
    const result = await this.request<{ success: boolean; role: string; csrfToken: string; expiresAt?: number; twoFactorEnabled?: boolean; totpRequired?: boolean }>('/api/v1/admin/session');
    if (version !== this.sessionVersion) throw new Error('管理会话已改变，请重新加载');
    if (result.success !== true || result.role !== 'admin' || typeof result.csrfToken !== 'string' || !result.csrfToken) {
      this.clearSession(); this.onUnauthorized?.();
      throw new Error('管理会话无效');
    }
    if(this.csrfToken!==result.csrfToken)this.authenticatedUsername=null;
    this.csrfToken = result.csrfToken;
    this.twoFactorEnabled=result.twoFactorEnabled;this.totpRequired=result.totpRequired===true;
    clearTimeout(this.expiryTimer);
    if(result.expiresAt!==undefined){
      if(!Number.isFinite(result.expiresAt)||result.expiresAt*1000<=Date.now()){this.clearSession();this.onUnauthorized?.();throw new Error('管理会话已到期');}
      this.expiresAt=result.expiresAt*1000;
      this.expiryTimer=setTimeout(()=>{this.clearSession();this.onUnauthorized?.();},Math.min(2147483647,this.expiresAt-Date.now()));
    }
    return result;
  }

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
    const cards: AdminCardItem[] = [];
    for (let offset = 0; ; offset += 500) {
      const page = await this.request<AdminCardsResponse>(`/api/v1/admin/cards?offset=${offset}&limit=500`);
      if (!page.success) throw new Error('卡密列表读取失败');
      cards.push(...page.cards);
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

  async getFinancials(): Promise<AdminFinancials> {
    return this.request('/api/v1/admin/financials');
  }

  async getTraces(limit = 100): Promise<{ success: boolean; traces: Array<Record<string, unknown>> }> {
    return this.request(`/api/v1/admin/traces?limit=${Math.min(500, Math.max(1, limit))}`);
  }

  async pruneTraces(cutoffSecs: number): Promise<{ success: boolean; pruned: number }> {
    return this.request('/api/v1/admin/traces/prune', {
      method: 'POST',
      body: JSON.stringify({ cutoffSecs }),
    });
  }

  async batchCards(count: number, groupId = 'group-pro-plus', templateId = 'tier-2000', note?: string): Promise<{ success: boolean; cards: GeneratedCard[] }> {
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

export interface CommercialConfig {
  settings?: FinancialSettings;
  revision: string;
  groups: Array<Record<string, unknown>>;
  models: Array<Record<string, unknown>>;
  rate_cards: Array<Record<string, unknown>>;
  versions: Array<Record<string, unknown>>;
  audit: Array<Record<string, unknown>>;
}

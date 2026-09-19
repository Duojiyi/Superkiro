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
  accessToken: string;
  tokenType: string;
  expiresIn: number;
  expiresAt: number;
}

export interface AdminFinancials {
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
  status: 'active' | 'unactivated' | 'frozen' | 'banned' | 'voided' | 'expired';
  creditTotal: number;
  creditUsed: number;
  availableCredits: number;
  pointsTotal: number;
  pointsAvailable: number;
  boundDevices: string[];
  maxDevices: number;
  activatedAt?: number;
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
  private adminKey: string;
  private sessionToken: string;

  constructor(baseUrl: string = '', adminKey: string = '') {
    this.baseUrl = baseUrl;
    // Keep administrator credentials only for the lifetime of this page.
    this.adminKey = adminKey;
    this.sessionToken = '';
  }

  setAdminKey(key: string) {
    this.adminKey = key;
  }

  async establishSession(): Promise<AdminSessionResponse> {
    const res = await fetch(`${this.baseUrl}/api/v1/admin/session`, {
      method: 'POST',
      headers: { 'x-admin-key': this.adminKey, Accept: 'application/json' },
    });
    if (!res.ok) throw new Error('未授权：管理员密钥无效');
    const session = (await res.json()) as AdminSessionResponse;
    this.sessionToken = session.accessToken;
    this.adminKey = '';
    return session;
  }

  clearSession() {
    this.sessionToken = '';
    this.adminKey = '';
  }

  async logout(all: boolean = false): Promise<void> {
    try {
      if (this.sessionToken) {
        await this.request('/api/v1/admin/session/revoke', {
          method: 'POST',
          body: JSON.stringify({ all }),
        });
      }
    } catch (_) {
      // Clean up locally regardless of network outcome
    } finally {
      this.clearSession();
    }
  }

  getAdminKey(): string {
    return this.adminKey;
  }

  private async request<T>(path: string, options: RequestInit = {}): Promise<T> {
    const headers = new Headers(options.headers || {});
    if (this.sessionToken) {
      headers.set('Authorization', `Bearer ${this.sessionToken}`);
    }
    headers.set('Accept', 'application/json');
    if (options.body !== undefined) headers.set('Content-Type', 'application/json');

    const res = await fetch(`${this.baseUrl}${path}`, {
      ...options,
      headers,
    });

    if (!res.ok) {
      if (res.status === 401) {
        throw new Error('未授权：请配置有效管理员密钥 (x-admin-key)');
      }
      const errJson = await res.json().catch(() => ({}));
      throw new Error(errJson.error || `请求失败 (${res.status})`);
    }

    return await res.json();
  }

  async checkAuth(): Promise<{ success: boolean; role: string }> {
    if (!this.sessionToken && this.adminKey) await this.establishSession();
    return this.request('/api/v1/admin/me');
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
    return this.request('/api/v1/admin/cards?offset=0&limit=500');
  }

  async updateCardStatus(
    cardId: string,
    action: 'freeze' | 'unfreeze' | 'ban',
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
    reason?: string
  ): Promise<AdminCardAdjustResponse> {
    return this.request('/api/v1/admin/cards/adjust', {
      method: 'POST',
      body: JSON.stringify({ cardId, deltaPoints, reason }),
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

  async batchCards(count: number, groupId = 'group-pro-plus', templateId = 'tier-2000'): Promise<{ success: boolean; cards: GeneratedCard[] }> {
    return this.request('/api/v1/admin/cards/batch', {
      method: 'POST',
      body: JSON.stringify({ count, groupId, templateId, maxDevices: 1 }),
    });
  }

  async exportLedger(format: 'json' | 'csv'): Promise<Blob> {
    const response = await fetch(`${this.baseUrl}/api/v1/admin/exports/ledger.${format}`, {
      headers: { Authorization: `Bearer ${this.sessionToken}`, Accept: format === 'csv' ? 'text/csv' : 'application/json' },
    });
    if (!response.ok) throw new Error(`导出失败 (${response.status})`);
    return response.blob();
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
  revision: string;
  groups: Array<Record<string, unknown>>;
  models: Array<Record<string, unknown>>;
  rate_cards: Array<Record<string, unknown>>;
  versions: Array<Record<string, unknown>>;
  audit: Array<Record<string, unknown>>;
}

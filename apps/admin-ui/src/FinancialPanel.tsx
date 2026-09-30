import {useEffect, useState} from 'react';
import {adminApi, type AdminFinancials} from './api';
import {formatDateTime} from './format';
import {readSettings, type PricingSettings} from './officialPricing';

/**
 * 结算参数, read only: the face value and rates the yuan figures here are computed with. They are
 * changed in 模型与定价 › 定价设置, where a new face value previews every model it touches and
 * reprices the official prices in the same publication.
 */
export default function FinancialPanel(props: {data?: AdminFinancials | null; onPublished: () => Promise<void>; onDirtyChange: (dirty: boolean) => void; onBusyChange: (busy: boolean) => void; refreshEpoch?: number}) {
  const {onDirtyChange, refreshEpoch = 0} = props;
  const [settings, setSettings] = useState<{view: PricingSettings; updated: number} | null>(null);
  const [failed, setFailed] = useState(false);
  // Nothing here is edited: leaving the page never needs a confirmation.
  useEffect(() => {onDirtyChange(false);}, [onDirtyChange]);
  useEffect(() => {
    let alive = true;
    adminApi.getCommercialConfig().then(result => {
      if (!alive) return;
      const raw = result.config?.settings;
      setSettings(raw ? {view: readSettings(raw), updated: Number(raw.rate_updated_at_secs ?? 0)} : null); setFailed(!raw);
    }).catch(() => {if (alive) setFailed(true);});
    return () => {alive = false;};
  }, [refreshEpoch]);
  const view = settings?.view;
  return <section className="panel settings-panel" aria-label="结算参数">
    <div className="panel-head">
      <h3>结算参数</h3>
      {settings?.updated ? <span className="muted">上次更新 {formatDateTime(settings.updated)}</span> : null}
    </div>
    {view ? <ul className="settings-facts">
      <li>积分面值 <b>{view.face ?? '—'}</b> 元/积分{view.face !== null ? `（1000 积分 = ¥${+(view.face * 1000).toFixed(4)}）` : ''}</li>
      <li>官方价 $1 = <b>¥{view.usdCny}</b></li>
      <li>旧版成本加成版本使用的汇率 <b>{view.legacyRate ?? '—'}</b> CNY/USD</li>
    </ul> : <p className="muted">{failed ? '结算参数没有读到，请刷新后再看' : '正在读取…'}</p>}
    <p className="muted">在“模型与定价 › 定价设置”里修改：改面值会先预览每个受影响的模型，并在同一次发布里重算按官方价定的价格。</p>
  </section>;
}

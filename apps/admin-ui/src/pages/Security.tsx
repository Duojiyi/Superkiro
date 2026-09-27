// 安全与审计: the saved ledger's size and its archive, the security checklist, the current
// session, and who changed the configuration.
import {useEffect, useRef, useState} from 'react';
import {adminApi, type AdminStats, type LedgerArchive} from '../api';
import {ask} from '../components/confirm';
import {IconCheck, IconInfo, IconWarning} from '../components/icons';
import {toast} from '../components/toast';
import {IdCell, TableState} from '../components/ui';
import {formatBytes, formatCount, formatDateTime, formatFullDateTime, formatSessionLeft} from '../format';
import {explainRefusal, isRefusal} from '../refusal';
import {storageLevel} from '../status';
import type {Refresh, Row, WriteGuards} from '../types';

const DAY_MS = 86_400_000;
/** A local date as the date input takes it: 2026-08-28. */
const dateText = (ms: number) => {const date = new Date(ms); return `${date.getFullYear()}-${String(date.getMonth() + 1).padStart(2, '0')}-${String(date.getDate()).padStart(2, '0')}`;};
const errorText = (error: unknown) => (error instanceof Error ? error.message : String(error));

/**
 * 存储与账本: the saved billing state against the size at which every save, and so every request,
 * fails; and 归档账本, which moves ledger entries before a date into an archive file beside it,
 * keeping every card's balance and quotas.
 */
function StoragePanel({stats, refresh, guards}: {stats: AdminStats | null; refresh: Refresh; guards: WriteGuards}) {
  const storage = storageLevel(stats);
  const alive = useRef(true);
  useEffect(() => {alive.current = true; return () => {alive.current = false;};}, []);
  const today = dateText(Date.now());
  const [cutoff, setCutoff] = useState(() => dateText(Date.now() - 30 * DAY_MS));
  const [archiving, setArchiving] = useState(false);
  const [outcome, setOutcome] = useState<{archive: LedgerArchive} | {error: string} | null>(null);
  const archive = async () => {
    const before = Math.floor(new Date(`${cutoff}T00:00:00`).getTime() / 1000);
    if (guards.writing.current || archiving) return;
    if (!Number.isFinite(before) || cutoff > today) {setOutcome({error: '请选择今天或更早的日期'}); return;}
    const answer = await ask({
      title: `归档 ${cutoff} 之前的账本记录？`,
      facts: [...(storage ? [`账本存储现在 ${formatBytes(storage.bytes)} / ${formatBytes(storage.ceiling)}`] : []),
        `${cutoff} 00:00 之前的每次扣费、调账和充值记录移出保存的账本，写进服务器上的归档文件（和账本一样加密）`],
      consequence: '卡内余额和用量额度都不变，卡的调账记录也仍在。财务对账和导出的账本只统计留下的记录，归档的部分不再计入。',
      confirmLabel: '归档',
    });
    if (!answer.confirmed || guards.writing.current || !alive.current) return;
    guards.writing.current = true; setArchiving(true); setOutcome(null);
    try {
      const result = await adminApi.archiveLedger(before);
      if (result.success !== true || !result.receipt) throw new Error('服务器未确认归档结果');
      if (alive.current) setOutcome({archive: result});
      toast.success(`已归档 ${formatCount(result.receipt.drained_entries_count)} 条账本记录`);
      void refresh({keepSelection: true});
    } catch (error) {
      // The same entries cannot be archived twice: after an unconfirmed result, trying again only finds what is left.
      if (alive.current) setOutcome({error: isRefusal(error) ? `没有归档：${explainRefusal(errorText(error))}`
        : `没收到归档结果（${errorText(error)}）。请点“刷新”核对账本存储的大小；再试一次只会归档还留着的记录。`});
    } finally {guards.writing.current = false; if (alive.current) setArchiving(false);}
  };
  const receipt = outcome && 'archive' in outcome ? outcome.archive : null;
  const share = (bytes: number) => `${storage ? Math.min(100, bytes / storage.ceiling * 100) : 0}%`;
  return <section className="panel" aria-label="存储与账本">
    <h3>存储与账本</h3>
    {storage ? <>
      <p className="storage-line"><b>{formatBytes(storage.bytes)}</b><span className="muted">/ {formatBytes(storage.ceiling)}</span>
        {storage.level === 'now' ? <span className="is-danger">已过请现在归档的 {formatBytes(storage.urgent)}：到上限时所有请求都会被拒绝</span>
          : storage.level === 'soon' ? <span className="is-warning">已过建议归档的 {formatBytes(storage.warning)}</span> : <span className="muted">正常</span>}</p>
      <div className="storage-meter" role="meter" aria-label="账本存储" aria-valuemin={0} aria-valuemax={storage.ceiling} aria-valuenow={storage.bytes}
        aria-valuetext={`${formatBytes(storage.bytes)} / ${formatBytes(storage.ceiling)}`}>
        <span className={`is-${storage.level}`} style={{width: share(storage.bytes)}}/>
        <i style={{left: share(storage.warning)}} title={`${formatBytes(storage.warning)}：建议归档`}/>
        <i style={{left: share(storage.urgent)}} title={`${formatBytes(storage.urgent)}：请现在归档`}/>
      </div>
      <p className="storage-note muted">保存的账本到 {formatBytes(storage.ceiling)} 时每次保存都会失败，所有请求都会被拒绝。{formatBytes(storage.warning)} 起建议归档旧账本，{formatBytes(storage.urgent)} 起请现在归档。</p>
    </> : <p className="muted">服务器没有报告账本存储的大小</p>}
    <div className="storage-archive">
      <label className="inline-field"><span>归档这天之前的记录</span>
        <input type="date" aria-label="归档日期" max={today} value={cutoff} disabled={archiving} onChange={event => setCutoff(event.target.value)}/></label>
      <button type="button" className="btn" disabled={archiving || !cutoff} onClick={() => void archive()}>{archiving ? '正在归档…' : '归档账本…'}</button>
    </div>
    {receipt && <p role="status" className="note-info storage-receipt">
      {`已归档 ${formatCount(receipt.receipt.drained_entries_count)} 条记录（${formatFullDateTime(receipt.receipt.before_ts_secs).slice(0, 10)} 之前）`}
      {` · 账本存储 ${formatBytes(receipt.stateBytesBefore)} → ${formatBytes(receipt.stateBytesAfter)} · 归档文件 `}<span className="mono">{receipt.receipt.archive_file}</span>
      {' · SHA-256 '}<span className="mono" title={receipt.receipt.sha256_checksum}>{receipt.receipt.sha256_checksum.slice(0, 12)}…</span></p>}
    {outcome && 'error' in outcome && <p role="alert" className="form-error">{outcome.error}</p>}
  </section>;
}

export default function SecurityPage({operator, keyCount, stats, refresh, guards, onLogout, onLogoutAll}: {
  operator: string | null;
  keyCount: number | null;
  stats: AdminStats | null;
  refresh: Refresh;
  guards: WriteGuards;
  onLogout: () => void;
  onLogoutAll: () => void;
}) {
  const [audit, setAudit] = useState<Row[]>([]);
  const [auditState, setAuditState] = useState<'loading' | 'loaded' | 'failed'>('loading');
  const [attempt, setAttempt] = useState(0);
  useEffect(() => {
    let current = true;
    setAuditState('loading');
    adminApi.getCommercialConfig().then(result => {
      if (!result.success) throw new Error('未确认配置审计');
      if (current) {setAudit(result.config.audit); setAuditState('loaded');}
    }).catch(() => {if (current) setAuditState('failed');});
    return () => {current = false;};
  }, [attempt]);
  const [now, setNow] = useState(() => Date.now());
  useEffect(() => {const timer = setInterval(() => setNow(Date.now()), 1000); return () => clearInterval(timer);}, []);
  const twoFactor = adminApi.twoFactorEnabled;
  const expiresAt = adminApi.sessionExpiresAt;

  return <div className="page-stack">
    <StoragePanel stats={stats} refresh={refresh} guards={guards}/>
    <div className="two-columns">
      <section className="panel">
        <h3>安全状态</h3>
        <ul className="checklist">
          <li className="is-ok"><IconCheck/>API 密钥加密存储{keyCount !== null ? `（${keyCount} 把）` : ''}</li>
          {twoFactor === true ? <li className="is-ok"><IconCheck/>双因素验证已启用</li>
            : twoFactor === false ? <li className="is-neutral"><IconInfo/>登录使用用户名和密码（未启用双因素验证）</li>
            : <li className="is-unknown"><IconWarning/>双因素验证状态未知</li>}
          <li className="is-ok"><IconCheck/>30 分钟未操作自动退出（最长 8 小时）</li>
        </ul>
      </section>
      <section className="panel">
        <h3>当前登录</h3>
        <p className="session-line"><b>{operator ?? '管理员'}</b>{expiresAt > 0 && <span className="muted"> · 剩余 {formatSessionLeft(expiresAt - now)}（{formatFullDateTime(expiresAt / 1000).slice(11, 16)} 到期）</span>}</p>
        <div className="button-row">
          <button type="button" className="btn" onClick={onLogout}>退出登录</button>
          <button type="button" className="btn btn-danger" onClick={onLogoutAll}>下线全部会话</button>
        </div>
      </section>
    </div>
    <section className="panel">
      <h3>配置变更记录</h3>
      <div className="table-scroll"><table className="table">
        <thead><tr><th>时间</th><th>操作人</th><th>原因</th><th>版本</th></tr></thead>
        <tbody>
          {audit.map((row, index) => <tr key={index}>
            <td title={formatFullDateTime(row.created_at_secs)}>{formatDateTime(row.created_at_secs)}</td>
            <td>{String(row.operator ?? '—')}</td>
            <td className="col-reason" title={String(row.reason ?? '')}><span className="clip clip-reason">{String(row.reason ?? '—')}</span></td>
            <td><IdCell value={row.revision} kind="hash"/></td>
          </tr>)}
          {!audit.length && <TableState colSpan={4} loading={auditState === 'loading'} failed={auditState === 'failed'} empty="暂无记录" onRetry={() => setAttempt(value => value + 1)}/>}
        </tbody>
      </table></div>
    </section>
  </div>;
}

// 安全与审计: the security checklist, the current session, and who changed the configuration.
import {useEffect, useState} from 'react';
import {adminApi} from '../api';
import {IconCheck, IconWarning} from '../components/icons';
import {IdCell, TableState} from '../components/ui';
import {formatDateTime, formatFullDateTime, formatSessionLeft} from '../format';
import type {Row} from '../types';

export default function SecurityPage({operator, keyCount, onLogout, onLogoutAll}: {
  operator: string | null;
  keyCount: number | null;
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
    <div className="two-columns">
      <section className="panel">
        <h3>安全状态</h3>
        <ul className="checklist">
          <li className="is-ok"><IconCheck/>API 密钥加密存储{keyCount !== null ? `（${keyCount} 把）` : ''}</li>
          {twoFactor === true ? <li className="is-ok"><IconCheck/>双因素验证已启用</li>
            : twoFactor === false ? <li className="is-warning"><IconWarning/>双因素验证未启用（在服务器配置中开启）</li>
            : <li className="is-unknown"><IconWarning/>双因素验证状态未知</li>}
          <li className="is-ok"><IconCheck/>会话 15 分钟自动失效</li>
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

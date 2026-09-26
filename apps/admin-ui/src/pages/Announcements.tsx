// 公告管理: the list, and one editor with a live preview. Publishing asks once, showing the
// rendered notice; a publish without a confirmed result locks further publishing until
// the list has been refreshed and reviewed.
import {Fragment, useEffect, useRef, useState} from 'react';
import {adminApi, AdminApiError, type AdminAnnouncement} from '../api';
import {confirmAction} from '../components/confirm';
import {toast} from '../components/toast';
import {StatusBadge, TableState} from '../components/ui';
import {formatDateTime, formatFullDateTime, formatRemaining, formatShortDate} from '../format';
import {NOTICE_LEVEL, noticeStatusView} from '../status';
import type {Refresh, ReportError, WriteGuards} from '../types';

const DRAFT_KEY = 'admin-announcement-draft:v1';
const PENDING_KEY = 'admin-pending-announcement:v1';
const DURATIONS = [1, 3, 7, 30];
type Level = AdminAnnouncement['level'];

const refused = (error: unknown) => error instanceof AdminApiError && [400, 403, 404, 409, 413, 422].includes(error.status);
const errorText = (error: unknown) => (error instanceof Error ? error.message : String(error));

function NoticePreview({title, content, level}: {title: string; content: string; level: Level}) {
  return <div className={`notice-preview level-${level}`}>
    <strong className={title ? undefined : 'placeholder'}>{title || '标题'}</strong>
    <p className={`preview-content${content ? '' : ' placeholder'}`}>{content || '正文'}</p>
  </div>;
}

export default function AnnouncementsPage({announcements, loading, failed, refresh, guards, reportError, updateAnnouncements}: {
  announcements: AdminAnnouncement[];
  loading: boolean;
  failed: boolean;
  refresh: Refresh;
  guards: WriteGuards;
  reportError: ReportError;
  updateAnnouncements: (announcements: AdminAnnouncement[]) => void;
}) {
  const {writing} = guards;
  const alive = useRef(true);
  useEffect(() => {alive.current = true; return () => {alive.current = false;};}, []);
  const [saved] = useState<Record<string, unknown>>(() => {
    try {const value = JSON.parse(sessionStorage.getItem(DRAFT_KEY) || '{}'); return value && typeof value === 'object' ? value : {};}
    catch {return {};}
  });
  const [title, setTitle] = useState(() => typeof saved.title === 'string' ? saved.title : '');
  const [level, setLevel] = useState<Level>(() => saved.level === 'warning' || saved.level === 'critical' ? saved.level : 'info');
  const [content, setContent] = useState(() => typeof saved.content === 'string' ? saved.content : '');
  const [days, setDays] = useState(() => DURATIONS.includes(Number(saved.days)) ? Number(saved.days) : 7);
  useEffect(() => {
    try {
      if (title || content) sessionStorage.setItem(DRAFT_KEY, JSON.stringify({title, level, content, days}));
      else sessionStorage.removeItem(DRAFT_KEY);
    } catch {/* A draft that cannot be kept is only lost at a session end. */}
  }, [title, level, content, days]);
  const [recovery, setRecovery] = useState<'refresh' | 'review' | null>(() => {
    try {return sessionStorage.getItem(PENDING_KEY) ? 'refresh' : null;} catch {return 'refresh';}
  });
  const [checking, setChecking] = useState(false);
  const checkingNotices = useRef(false);
  const [busy, setBusy] = useState(false);
  const [formError, setFormError] = useState('');
  const [expanded, setExpanded] = useState<string | null>(null);
  const nowSecs = Date.now() / 1000;

  const refreshForReview = async () => {
    if (checkingNotices.current) return;
    checkingNotices.current = true; setChecking(true); setRecovery('refresh');
    try {
      const result = await adminApi.getAnnouncements();
      if (!result.success) throw new Error('服务端未确认公告列表');
      if (!alive.current) return;
      updateAnnouncements(result.announcements); setRecovery('review'); reportError('');
    } catch {
      if (alive.current) reportError('公告核对刷新失败，仍禁止发布，请重试。');
    } finally {checkingNotices.current = false; if (alive.current) setChecking(false);}
  };
  const release = async () => {
    if (!(await confirmAction({title: '确认已核对公告列表？', consequence: '如果公告已经发布，请不要重复发布。解除后不会自动提交。', confirmLabel: '继续'}))) return;
    try {sessionStorage.removeItem(PENDING_KEY); setRecovery(null); reportError('');}
    catch {reportError('无法清除待核对记录，仍禁止发布。');}
  };

  const publish = async () => {
    if (busy || writing.current || recovery) return;
    const cleanTitle = title.trim(), cleanContent = content.trim();
    if (!cleanTitle || !cleanContent) {setFormError('公告标题和内容不能为空'); return;}
    if ([...cleanTitle].length > 256 || [...cleanContent].length > 20000) {setFormError('公告标题最多 256 字，正文最多 20,000 字'); return;}
    setFormError('');
    const confirmed = await confirmAction({
      title: '发布公告？',
      body: <NoticePreview title={cleanTitle} content={cleanContent} level={level}/>,
      consequence: `对全部用户可见 · ${days} 天后自动下线`,
      confirmLabel: '发布',
    });
    if (!confirmed || !alive.current || writing.current || recovery) return;
    try {
      setBusy(true); reportError(''); writing.current = true;
      sessionStorage.setItem(PENDING_KEY, new Date().toISOString());
      const response = await adminApi.createAnnouncement(cleanTitle, cleanContent, level, 86400 * days);
      if (!response.success) throw new Error('服务端未确认公告发布');
      sessionStorage.removeItem(PENDING_KEY);
      toast.success('已发布公告');
      if (alive.current) {setTitle(''); setContent('');}
      void refresh();
    } catch (error) {
      if (refused(error)) {sessionStorage.removeItem(PENDING_KEY); reportError(`服务端已拒绝，公告未发布：${errorText(error)}`); return;}
      if (alive.current) setRecovery('refresh');
      reportError(`没收到发布结果（${errorText(error)}），可能已经发布，请先核对列表，不要重复提交。`);
    } finally {writing.current = false; if (alive.current) setBusy(false);}
  };

  const withdraw = async (notice: AdminAnnouncement) => {
    if (busy || writing.current) return;
    if (!(await confirmAction({title: `撤回「${notice.title}」？`, consequence: '客户端将不再显示（已打开的客户端稍后移除）。', confirmLabel: '撤回', danger: true}))) return;
    if (writing.current || !alive.current) return;
    try {
      setBusy(true); reportError(''); writing.current = true;
      const response = await adminApi.withdrawAnnouncement(notice.id);
      if (response.success !== true) throw new Error('服务端未确认撤回');
      toast.success(`已撤回「${notice.title}」`);
      void refresh();
    } catch (error) {
      reportError(`撤回结果未确认：${errorText(error)}。请刷新公告列表核对。`);
    } finally {writing.current = false; if (alive.current) setBusy(false);}
  };

  return <div className="page-stack">
    {recovery && <section className="recovery-panel" aria-label="公告发布结果核对">
      <div><h3>上次发布没收到结果</h3><p role="status">可能已经发布。核对列表后再继续。</p></div>
      <div className="button-row">
        <button type="button" className="btn btn-small" disabled={checking} onClick={() => void refreshForReview()}>{checking ? '正在刷新…' : '刷新列表'}</button>
        <button type="button" className="btn btn-small" disabled={checking || recovery !== 'review'} title={recovery !== 'review' ? '先刷新列表' : undefined} onClick={() => void release()}>已核对，继续</button>
      </div>
    </section>}

    <section className="panel">
      <div className="table-scroll"><table className="table">
        <thead><tr><th>标题</th><th className="col-status">等级</th><th>发布于</th><th>到期</th><th className="col-status">状态</th><th className="col-actions"><span className="sr-only">操作</span></th></tr></thead>
        <tbody>
          {announcements.map(notice => {
            const status = noticeStatusView(notice, nowSecs);
            const live = status.label === '生效中';
            const left = notice.expires_at ? formatRemaining(notice.expires_at) : null;
            const open = expanded === notice.id;
            return <Fragment key={notice.id}>
              <tr>
                <td className="col-title"><button type="button" className="link-button clip clip-title" aria-expanded={open} onClick={() => setExpanded(open ? null : notice.id)}>{notice.title}</button></td>
                <td className="col-status"><StatusBadge view={NOTICE_LEVEL[notice.level] ?? {label: notice.level, tone: 'outline'}}/></td>
                <td title={formatFullDateTime(notice.created_at)}>{formatDateTime(notice.created_at)}</td>
                <td title={notice.expires_at ? formatFullDateTime(notice.expires_at) : undefined}>{notice.expires_at ? <>{formatShortDate(notice.expires_at)}{live && left && <span className={`remaining is-${left.tone}`}>（{left.text}）</span>}</> : '—'}</td>
                <td className="col-status"><StatusBadge view={status}/></td>
                <td className="col-actions">{live && <button type="button" className="btn-text is-danger" disabled={busy} onClick={() => void withdraw(notice)}>撤回</button>}</td>
              </tr>
              {open && <tr className="detail-row"><td colSpan={6}><p className="preview-content">{notice.content}</p></td></tr>}
            </Fragment>;
          })}
          {!announcements.length && <TableState colSpan={6} loading={loading} failed={failed} empty="还没有公告" onRetry={() => void refresh()}/>}
        </tbody>
      </table></div>
    </section>

    <div className="two-columns">
      <section className="panel">
        <h3>新公告</h3>
        <div className="form-grid">
          <label className="field"><span className="field-label">标题</span>
            <input aria-label="公告标题" value={title} placeholder="例：9 月 28 日凌晨服务维护" onChange={event => setTitle(event.target.value)}/></label>
          <div className="form-grid form-grid-2">
            <label className="field"><span className="field-label">等级</span>
              <select aria-label="公告等级" value={level} onChange={event => setLevel(event.target.value as Level)}>
                <option value="info">普通</option><option value="warning">预警</option><option value="critical">紧急</option>
              </select></label>
            <label className="field"><span className="field-label">有效期</span>
              <select aria-label="有效期" value={days} onChange={event => setDays(Number(event.target.value))}>
                {DURATIONS.map(value => <option key={value} value={value}>{value} 天</option>)}
              </select></label>
          </div>
          <label className="field"><span className="field-label">正文</span>
            <textarea rows={6} aria-label="正文内容" value={content} onChange={event => setContent(event.target.value)}/></label>
        </div>
        {formError && <p role="alert" className="form-error">{formError}</p>}
        <div className="editor-actions"><div className="button-row">
          <button type="button" className="btn btn-primary" disabled={busy || !!recovery} title={recovery ? '上次发布的结果未确认，请先核对' : undefined} onClick={() => void publish()}>{busy ? '发布中…' : '发布'}</button>
        </div></div>
      </section>
      <section className="panel">
        <h3>预览</h3>
        <NoticePreview title={title.trim()} content={content.trim()} level={level}/>
        <p className="muted preview-note">对全部用户可见 · {days} 天后自动下线</p>
      </section>
    </div>
  </div>;
}

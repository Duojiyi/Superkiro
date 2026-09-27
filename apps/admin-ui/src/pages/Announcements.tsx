// 公告管理: the list (scheduled and shown ones; ended and withdrawn on request), one editor with a
// preview that looks as the customer's client shows it, and an edit for one already published.
// An announcement is shown from its start to its end, to every customer or the cards of some
// groups. Publishing asks once, showing the rendered notice; a publish or an edit without a
// confirmed result locks further publishing until the list has been refreshed and reviewed.
import {Fragment, useEffect, useLayoutEffect, useRef, useState} from 'react';
import {adminApi, AdminApiError, type AdminAnnouncement} from '../api';
import {confirmAction} from '../components/confirm';
import {Modal} from '../components/modal';
import {toast} from '../components/toast';
import {StatusBadge, TableState} from '../components/ui';
import {formatFullDateTime, formatRemaining} from '../format';
import {AUDIENCE_NOTE, audienceText, CLIENT_LEVEL, editText, explainNotice, MAX_AUDIENCE, momentText, NOTICE_DAYS, NOTICE_STATUS, noticeEdit, noticeStatus, noticeWindow,
  reachText, UNCHANGED_503, windowText, type NoticeTiming} from '../notices';
import {NOTICE_LEVEL} from '../status';
import {minuteInput, minuteStart} from '../traceQuery';
import type {Refresh, ReportError, Row, WriteGuards} from '../types';

const DRAFT_KEY = 'admin-announcement-draft:v1';
const PENDING_KEY = 'admin-pending-announcement:v1';
type Level = AdminAnnouncement['level'];

const refused = (error: unknown) => error instanceof AdminApiError && [400, 403, 404, 409, 413, 422].includes(error.status);
// The server's own 503s that say nothing was changed: not a result to check.
const unchanged = (error: unknown) => error instanceof AdminApiError && error.status === 503 && UNCHANGED_503.test(error.message);
const errorText = (error: unknown) => (error instanceof Error ? error.message : String(error));
const inAnHour = () => minuteInput(new Date(Math.ceil(Date.now() / 3_600_000) * 3_600_000));

/** Splits text at code points into what fits the box, as the client pages a long notice. */
function paginate(text: string, fits: (text: string) => boolean): string[] {
  const chars = Array.from(text), pages: string[] = [];
  for (let start = 0; start < chars.length;) {
    let low = 1, high = chars.length - start;
    while (low < high) {const mid = Math.ceil((low + high) / 2); if (fits(chars.slice(start, start + mid).join(''))) low = mid; else high = mid - 1;}
    pages.push(chars.slice(start, start + low).join(''));
    start += low;
  }
  return pages.length ? pages : [''];
}

/** The notice as the customer's client shows it: its dialog, heading, level word, text in pages. */
function ClientPreview({title, content, level, compact = false}: {title: string; content: string; level: Level; /** In a confirmation: the whole text, scrolled, without pages. */ compact?: boolean}) {
  const text = `${title || '标题'}\n\n${content || '正文'}`;
  const viewport = useRef<HTMLDivElement>(null), probe = useRef<HTMLDivElement>(null);
  const [pages, setPages] = useState<string[]>([text]);
  const [page, setPage] = useState(0);
  useLayoutEffect(() => {
    const box = viewport.current, measure = probe.current;
    if (!box || !measure || compact) return;
    const layout = () => {
      if (!box.clientWidth || !box.clientHeight) {setPages([text]); return;}
      setPages(paginate(text, candidate => {measure.textContent = candidate; return measure.scrollHeight <= box.clientHeight && measure.scrollWidth <= box.clientWidth;}));
      setPage(0);
    };
    layout();
    const observer = typeof ResizeObserver === 'function' ? new ResizeObserver(layout) : null;
    observer?.observe(box);
    return () => observer?.disconnect();
  }, [text, compact]);
  const current = Math.min(page, pages.length - 1);
  return <div className={`client-notice${compact ? ' is-compact' : ''}`} role="group" aria-label="客户端预览">
    <div className="client-notice-heading"><span className="client-notice-title">公告</span><span className="client-notice-button" aria-hidden="true">关闭</span></div>
    <div className="client-notice-meta">第 1 / 1 条 · {CLIENT_LEVEL[level] ?? '通知'}</div>
    <div ref={viewport} className="client-notice-viewport">
      <div className={`client-notice-text${title && content ? '' : ' placeholder'}`}>{compact ? text : pages[current]}</div>
      <div ref={probe} className="client-notice-text client-notice-measure" aria-hidden="true"/>
    </div>
    {!compact && <div className="client-notice-pagination">
      <button type="button" className="client-notice-button" aria-label="预览上一页" disabled={current === 0} onClick={() => setPage(current - 1)}>上一页</button>
      <span>{current + 1} / {pages.length} 页</span>
      <button type="button" className="client-notice-button" aria-label="预览下一页" disabled={current >= pages.length - 1} onClick={() => setPage(current + 1)}>下一页</button>
    </div>}
  </div>;
}

/** The start, end and audience fields, for a new notice and for an edit. */
function WindowFields({timing, setTiming, audience, setAudience, groups, scheduling, disabled, editing = false}: {
  timing: NoticeTiming; setTiming: (next: NoticeTiming) => void;
  audience: string[] | null; setAudience: (next: string[] | null) => void;
  groups: Row[]; scheduling: boolean; disabled?: boolean;
  /** An edit: the start is a minute (it has one already), the end a minute or none. */
  editing?: boolean;
}) {
  const endValue = timing.end === 'days' ? String(timing.days) : timing.end;
  return <>
    <div className="form-grid form-grid-2">
      {editing ? <label className="field"><span className="field-label">开始</span>
        <input type="datetime-local" aria-label="开始时间" value={timing.startAt} disabled={disabled} onChange={event => setTiming({...timing, startAt: event.target.value})}/>
        <span className="field-hint">已开始的公告不用改；改到以后，到时候才显示</span></label>
      : <label className="field"><span className="field-label">开始</span>
        <select aria-label="开始" value={timing.start} disabled={disabled || !scheduling} onChange={event => setTiming({...timing, start: event.target.value as NoticeTiming['start'], startAt: timing.startAt || inAnHour()})}>
          <option value="now">立即</option><option value="at">定时</option>
        </select>
        {timing.start === 'at' && <input type="datetime-local" aria-label="开始时间" value={timing.startAt} disabled={disabled} onChange={event => setTiming({...timing, startAt: event.target.value})}/>}</label>}
      <label className="field"><span className="field-label">{editing ? '结束' : '有效期'}</span>
        <select aria-label={editing ? '结束' : '有效期'} value={endValue} disabled={disabled} onChange={event => {
          const value = event.target.value;
          setTiming(value === 'at' || value === 'never' ? {...timing, end: value, endAt: timing.endAt || minuteInput(new Date(Date.now() + 86_400_000))} : {...timing, end: 'days', days: Number(value)});
        }}>
          {!editing && NOTICE_DAYS.map(value => <option key={value} value={value}>{value} 天</option>)}
          {(scheduling || editing) && <><option value="at">指定结束时间</option><option value="never">一直显示（直到撤回）</option></>}
        </select>
        {timing.end === 'at' && <input type="datetime-local" aria-label="结束时间" value={timing.endAt} disabled={disabled} onChange={event => setTiming({...timing, endAt: event.target.value})}/>}</label>
    </div>
    <div className="field"><span className="field-label">对象</span>
      <select aria-label="对象" value={audience ? 'groups' : 'all'} disabled={disabled || !scheduling} onChange={event => setAudience(event.target.value === 'groups' ? [] : null)}>
        <option value="all">全部客户</option><option value="groups">指定分组</option>
      </select>
      {audience && <fieldset className="audience-groups" aria-label="分组">
        {groups.map(group => <label key={String(group.id)} className="check-field"><input type="checkbox" checked={audience.includes(String(group.id))} disabled={disabled}
          onChange={event => setAudience(event.target.checked ? [...audience, String(group.id)] : audience.filter(id => id !== group.id))}/>{String(group.name ?? group.id)}</label>)}
        {!groups.length && <span className="muted">分组配置没有加载</span>}
      </fieldset>}
      {audience && <span className="field-hint audience-note">{AUDIENCE_NOTE}</span>}
    </div>
    {!scheduling && <p className="muted notice-legacy">服务器还不支持定时、指定结束时间和按分组发布：公告立即对全部客户显示，按有效期自动下线。</p>}
  </>;
}

/** What is wrong with the audience chosen, if anything. */
const audienceProblem = (audience: string[] | null) => audience && !audience.length ? '请至少选一个分组' : audience && audience.length > MAX_AUDIENCE ? `最多选 ${MAX_AUDIENCE} 个分组` : '';

export default function AnnouncementsPage({announcements, groups = [], scheduling = true, loading, failed, refresh, guards, reportError, updateAnnouncements}: {
  announcements: AdminAnnouncement[];
  /** For the audience and its names. */
  groups?: Row[];
  /** The server takes a start, an end, an audience and edits (newer servers). */
  scheduling?: boolean;
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
  const [timing, setTiming] = useState<NoticeTiming>(() => {
    const kept = saved.timing && typeof saved.timing === 'object' ? saved.timing as Partial<NoticeTiming> : {};
    return {start: kept.start === 'at' ? 'at' : 'now', startAt: typeof kept.startAt === 'string' ? kept.startAt : '',
      end: kept.end === 'at' || kept.end === 'never' ? kept.end : 'days', days: NOTICE_DAYS.includes(Number(kept.days ?? saved.days)) ? Number(kept.days ?? saved.days) : 7, endAt: typeof kept.endAt === 'string' ? kept.endAt : ''};
  });
  const [audience, setAudience] = useState<string[] | null>(() => Array.isArray(saved.audience) ? (saved.audience as unknown[]).map(String) : null);
  useEffect(() => {
    try {
      if (title || content) sessionStorage.setItem(DRAFT_KEY, JSON.stringify({title, level, content, days: timing.days, timing, audience}));
      else sessionStorage.removeItem(DRAFT_KEY);
    } catch {/* A draft that cannot be kept is only lost at a session end. */}
  }, [title, level, content, timing, audience]);
  const [recovery, setRecovery] = useState<'refresh' | 'review' | null>(() => {
    try {return sessionStorage.getItem(PENDING_KEY) ? 'refresh' : null;} catch {return 'refresh';}
  });
  const [checking, setChecking] = useState(false);
  const checkingNotices = useRef(false);
  const [busy, setBusy] = useState(false);
  const [formError, setFormError] = useState('');
  const [expanded, setExpanded] = useState<string | null>(null);
  // 显示已结束和已撤回: the whole list, read here.
  const [showAll, setShowAll] = useState(false);
  const [everything, setEverything] = useState<AdminAnnouncement[] | null>(null);
  useEffect(() => {
    if (!showAll) {setEverything(null); return;}
    let current = true;
    adminApi.getAnnouncements(true).then(result => {if (current && result.success) setEverything(result.announcements);}, () => {if (current) reportError('没能读取已结束和已撤回的公告，请重试');});
    return () => {current = false;};
  // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [showAll, announcements]);
  const [editing, setEditing] = useState<{notice: AdminAnnouncement; title: string; content: string; level: Level; timing: NoticeTiming; audience: string[] | null; error: string} | null>(null);
  const nowSecs = Date.now() / 1000;
  const groupName = (id: string) => String(groups.find(group => group.id === id)?.name ?? id);
  const shown = noticeWindow(scheduling ? timing : {...timing, start: 'now', end: 'days'}, nowSecs);
  const reach = reachText(scheduling ? audience ?? undefined : undefined, groupName);
  const groupsWrong = scheduling ? audienceProblem(audience) : '';
  const list = everything ?? announcements;

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
    if (!(await confirmAction({title: '确认已核对公告列表？', consequence: '如果公告已经发布（或修改已经生效），请不要重复提交。解除后不会自动提交。', confirmLabel: '继续'}))) return;
    try {sessionStorage.removeItem(PENDING_KEY); setRecovery(null); reportError('');}
    catch {reportError('无法清除待核对记录，仍禁止发布。');}
  };

  const publish = async () => {
    if (busy || writing.current || recovery) return;
    const cleanTitle = title.trim(), cleanContent = content.trim();
    if (!cleanTitle || !cleanContent) {setFormError('公告标题和内容不能为空'); return;}
    if ([...cleanTitle].length > 256 || [...cleanContent].length > 20000) {setFormError('公告标题最多 256 字，正文最多 20,000 字'); return;}
    if (shown.error) {setFormError(shown.error); return;}
    if (groupsWrong) {setFormError(groupsWrong); return;}
    setFormError('');
    const confirmed = await confirmAction({
      title: '发布公告？',
      body: <ClientPreview title={cleanTitle} content={cleanContent} level={level} compact/>,
      facts: scheduling && audience?.length ? [AUDIENCE_NOTE] : undefined,
      consequence: `${reach} · ${windowText(shown, nowSecs)}`,
      confirmLabel: '发布',
    });
    if (!confirmed || !alive.current || writing.current || recovery) return;
    try {
      setBusy(true); reportError(''); writing.current = true;
      sessionStorage.setItem(PENDING_KEY, new Date().toISOString());
      const {ttlSecs, startsAtSecs, endsAtSecs} = shown;
      const response = await adminApi.createAnnouncement(cleanTitle, cleanContent, level, ttlSecs,
        scheduling ? {startsAtSecs, endsAtSecs, ...(audience?.length ? {audience} : {})} : {});
      if (!response.success) throw new Error('服务端未确认公告发布');
      sessionStorage.removeItem(PENDING_KEY);
      toast.success(startsAtSecs ? `已发布公告，${momentText(startsAtSecs, nowSecs)} 开始显示` : '已发布公告');
      if (alive.current) {setTitle(''); setContent('');}
      void refresh();
    } catch (error) {
      // Refused, or not saved by the server's own word: nothing was published, and another try is safe.
      if (refused(error) || unchanged(error)) {sessionStorage.removeItem(PENDING_KEY); reportError(refused(error) ? `服务端已拒绝，公告未发布：${explainNotice(errorText(error))}` : explainNotice(errorText(error))); return;}
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
      reportError(refused(error) ? `没有撤回：${explainNotice(errorText(error))}` : unchanged(error) ? explainNotice(errorText(error)) : `撤回结果未确认：${errorText(error)}。请刷新公告列表核对。`);
    } finally {writing.current = false; if (alive.current) setBusy(false);}
  };

  const startEdit = (notice: AdminAnnouncement) => {
    const start = notice.starts_at ?? notice.created_at;
    setEditing({notice, title: notice.title, content: notice.content, level: notice.level, audience: notice.audience?.length ? [...notice.audience] : null, error: '',
      timing: {start: 'at', startAt: minuteInput(new Date(start * 1000)), end: notice.expires_at ? 'at' : 'never', days: 7, endAt: notice.expires_at ? minuteInput(new Date(notice.expires_at * 1000)) : ''}});
  };
  const saveEdit = async () => {
    if (!editing || busy || writing.current || recovery) return;
    const {notice} = editing, cleanTitle = editing.title.trim(), cleanContent = editing.content.trim();
    const fail = (error: string) => setEditing(current => current && {...current, error});
    if (!cleanTitle || !cleanContent) return fail('公告标题和内容不能为空');
    if ([...cleanTitle].length > 256 || [...cleanContent].length > 20000) return fail('公告标题最多 256 字，正文最多 20,000 字');
    const groupsWrong = audienceProblem(editing.audience);
    if (groupsWrong) return fail(groupsWrong);
    // A start left as it was is not sent (it may have passed); the end is a minute or none.
    const originalStart = minuteInput(new Date((notice.starts_at ?? notice.created_at) * 1000));
    const startAt = editing.timing.startAt === originalStart ? undefined : minuteStart(editing.timing.startAt);
    if (startAt === null) return fail('请填写开始时间');
    const endAt = editing.timing.end === 'never' ? null : minuteStart(editing.timing.endAt);
    if (editing.timing.end === 'at' && endAt === null) return fail('请填写结束时间');
    const originalEnd = notice.expires_at ? minuteInput(new Date(notice.expires_at * 1000)) : '';
    const endsAtSecs = editing.timing.end === 'at' && editing.timing.endAt === originalEnd ? undefined : endAt;
    const start = startAt ?? notice.starts_at ?? notice.created_at;
    if (typeof endsAtSecs === 'number' && endsAtSecs <= Math.max(start, Math.floor(nowSecs))) return fail('结束时间要晚于开始时间和现在');
    const edit = noticeEdit(notice, {title: cleanTitle, content: cleanContent, level: editing.level, startsAtSecs: startAt, endsAtSecs, audience: editing.audience ?? []});
    if (!Object.keys(edit).length) return fail('没有修改');
    const changes = [edit.title !== undefined && '标题', edit.content !== undefined && '正文', edit.level !== undefined && '等级', edit.startsAtSecs !== undefined && '开始时间',
      edit.endsAtSecs !== undefined && '结束时间', edit.audience !== undefined && '对象'].filter(Boolean).join('、');
    const confirmed = await confirmAction({
      title: `修改「${notice.title}」？`,
      body: <ClientPreview title={cleanTitle} content={cleanContent} level={editing.level} compact/>,
      facts: [`改了${changes}`, windowText({startsAtSecs: start > nowSecs ? start : undefined, endsAtSecs: endsAtSecs === undefined ? notice.expires_at ?? undefined : endsAtSecs ?? undefined}, nowSecs),
        `对象：${audienceText(editing.audience ?? undefined, groupName)}`],
      consequence: '客户端下次拉取公告时显示修改后的内容；改过的公告会作为新公告再提醒一次。',
      confirmLabel: '保存修改',
    });
    if (!confirmed || !alive.current || writing.current) return;
    try {
      setBusy(true); reportError(''); writing.current = true;
      sessionStorage.setItem(PENDING_KEY, new Date().toISOString());
      const response = await adminApi.editAnnouncement({id: notice.id, ...edit});
      if (!response.success) throw new Error('服务端未确认修改');
      sessionStorage.removeItem(PENDING_KEY);
      toast.success(`已修改「${cleanTitle}」`);
      if (alive.current) setEditing(null);
      void refresh();
    } catch (error) {
      if (refused(error) || unchanged(error)) {sessionStorage.removeItem(PENDING_KEY); fail(refused(error) ? `服务端已拒绝，公告没有改动：${explainNotice(errorText(error))}` : explainNotice(errorText(error))); return;}
      if (alive.current) {setRecovery('refresh'); setEditing(null);}
      reportError(`没收到修改结果（${errorText(error)}），可能已经生效，请先核对列表，不要重复提交。`);
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

    <section className="panel" aria-label="公告列表">
      {scheduling && <div className="panel-head notices-head"><label className="check-field"><input type="checkbox" checked={showAll} onChange={event => setShowAll(event.target.checked)}/>显示已结束和已撤回</label></div>}
      <div className="table-scroll"><table className="table notices-table">
        <thead><tr><th>标题</th><th className="col-status">等级</th><th>对象</th><th>开始</th><th>结束</th><th className="col-status">状态</th><th className="col-actions"><span className="sr-only">操作</span></th></tr></thead>
        <tbody>
          {list.map(notice => {
            const stands = noticeStatus(notice, nowSecs), start = notice.starts_at ?? notice.created_at;
            const live = stands === 'active' || stands === 'scheduled';
            const left = notice.expires_at ? formatRemaining(notice.expires_at) : null;
            const open = expanded === notice.id;
            return <Fragment key={notice.id}>
              <tr>
                <td className="col-title"><button type="button" className="link-button clip clip-title" aria-expanded={open} onClick={() => setExpanded(open ? null : notice.id)}>{notice.title}</button></td>
                <td className="col-status"><StatusBadge view={NOTICE_LEVEL[notice.level] ?? {label: notice.level, tone: 'outline'}}/></td>
                <td className="col-audience" title={notice.audience?.length ? AUDIENCE_NOTE : undefined}>{audienceText(notice.audience, groupName)}</td>
                <td title={formatFullDateTime(start)}>{momentText(start, nowSecs)}{stands === 'scheduled' && <span className="remaining is-info">（{formatRemaining(start).text.replace('剩 ', '')}后）</span>}</td>
                <td title={notice.expires_at ? formatFullDateTime(notice.expires_at) : '一直显示，直到撤回'}>{notice.expires_at ? <>{momentText(notice.expires_at, nowSecs)}{stands === 'active' && left && <span className={`remaining is-${left.tone}`}>（{left.text}）</span>}</> : <span className="muted">不自动结束</span>}</td>
                <td className="col-status"><StatusBadge view={NOTICE_STATUS[stands]}/></td>
                <td className="col-actions"><span className="row-actions">
                  {live && scheduling && <button type="button" className="btn-text" disabled={busy || !!recovery} title={recovery ? '上次的结果未确认，请先核对' : undefined} onClick={() => startEdit(notice)}>编辑</button>}
                  {live && <button type="button" className="btn-text is-danger" disabled={busy} onClick={() => void withdraw(notice)}>撤回</button>}
                </span></td>
              </tr>
              {open && <tr className="detail-row"><td colSpan={7}>
                <p className="preview-content">{notice.content}</p>
                {notice.edits?.length ? <ul className="notice-edits" aria-label="修改记录">{notice.edits.map((edit, index) => <li key={index}>{editText(edit, nowSecs)}</li>)}</ul> : null}
              </td></tr>}
            </Fragment>;
          })}
          {!list.length && <TableState colSpan={7} loading={loading} failed={failed} empty={showAll ? '还没有公告' : '没有未开始或生效中的公告'} onRetry={() => void refresh()}/>}
        </tbody>
      </table></div>
    </section>

    <div className="two-columns">
      <section className="panel">
        <h3>新公告</h3>
        <div className="form-grid">
          <label className="field"><span className="field-label">标题</span>
            <input aria-label="公告标题" value={title} placeholder="例：9 月 28 日凌晨服务维护" onChange={event => setTitle(event.target.value)}/></label>
          <label className="field"><span className="field-label">等级</span>
            <select aria-label="公告等级" value={level} onChange={event => setLevel(event.target.value as Level)}>
              <option value="info">普通</option><option value="warning">预警</option><option value="critical">紧急</option>
            </select></label>
          <WindowFields timing={timing} setTiming={setTiming} audience={scheduling ? audience : null} setAudience={setAudience} groups={groups} scheduling={scheduling}/>
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
        <ClientPreview title={title.trim()} content={content.trim()} level={level}/>
        <p className="muted preview-note" aria-label="显示范围">{shown.error || groupsWrong || `${reach} · ${windowText(shown, nowSecs)}`}</p>
      </section>
    </div>

    {editing && <Modal label="编辑公告" onClose={() => setEditing(null)} busy={busy} className="dialog-form notice-editor">
      <h3 className="modal-title">编辑公告</h3>
      {editing.error && <p role="alert" className="form-error">{editing.error}</p>}
      <div className="form-grid">
        <label className="field"><span className="field-label">标题</span>
          <input aria-label="修改标题" value={editing.title} disabled={busy} onChange={event => setEditing({...editing, title: event.target.value, error: ''})}/></label>
        <label className="field"><span className="field-label">等级</span>
          <select aria-label="修改等级" value={editing.level} disabled={busy} onChange={event => setEditing({...editing, level: event.target.value as Level, error: ''})}>
            <option value="info">普通</option><option value="warning">预警</option><option value="critical">紧急</option>
          </select></label>
        <WindowFields timing={editing.timing} setTiming={timing => setEditing({...editing, timing, error: ''})} editing
          audience={editing.audience} setAudience={next => setEditing({...editing, audience: next, error: ''})} groups={groups} scheduling={scheduling} disabled={busy}/>
        <label className="field"><span className="field-label">正文</span>
          <textarea rows={6} aria-label="修改正文" value={editing.content} disabled={busy} onChange={event => setEditing({...editing, content: event.target.value, error: ''})}/></label>
      </div>
      {editing.notice.edits?.length ? <p className="muted">修改记录：{editing.notice.edits.map(edit => editText(edit, nowSecs)).join('；')}</p> : null}
      <div className="modal-actions">
        <button type="button" className="btn" disabled={busy} onClick={() => setEditing(null)}>取消</button>
        <button type="button" className="btn btn-primary" disabled={busy || !!recovery} onClick={() => void saveEdit()}>{busy ? '保存中…' : '保存修改'}</button>
      </div>
    </Modal>}
  </div>;
}

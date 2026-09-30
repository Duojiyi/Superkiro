import {TemplateDeliveryEditor} from '../components/TemplateDeliveryEditor';
import {useEffect, useRef, useState} from 'react';
import {adminApi, AdminApiError} from '../api';
import {confirmAction} from '../components/confirm';
import {toast} from '../components/toast';
import {Switch, TopbarActions} from '../components/ui';
import {MAX_TEMPLATE_ITEMS, pelicanIntent, newTemplateRule, sameTemplateRules, newTemplateVariant, parseTemplateRules, pelicanTemplateRule, templateDrafts, templateModels, templatePrice,
  type ResponseTemplateConfig, type ResponseTemplateRule, type TemplateRuleDraft, type TemplateVariantDraft} from '../responseTemplates';
import type {Row, WriteGuards} from '../types';

const message = (error: unknown) => error instanceof Error ? error.message : String(error);

export default function ResponseTemplatesPage({models, modelsFailed, refreshEpoch, guards, onDirtyChange, onBusyChange}: {
  models: Row[]; modelsFailed: boolean; refreshEpoch: number; guards: WriteGuards;
  onDirtyChange: (dirty: boolean) => void; onBusyChange: (busy: boolean) => void;
}) {
  const [config, setConfig] = useState<ResponseTemplateConfig | null>(null);
  const [drafts, setDrafts] = useState<TemplateRuleDraft[]>([]);
  const [selected, setSelected] = useState<string | null>(null);
  const [reason, setReason] = useState('');
  const [previewPrompt, setPreviewPrompt] = useState('');
  const [previewModel, setPreviewModel] = useState('');
  const [previewResult, setPreviewResult] = useState('');
  const [previewBusy, setPreviewBusy] = useState(false);
  const previewEpoch = useRef(0);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState('');
  const [blocked, setBlocked] = useState<'conflict' | 'uncertain' | ''>('');
  const [latest, setLatest] = useState<ResponseTemplateConfig | null>(null);
  const alive = useRef(true), reading = useRef(false), submitting = useRef(false);
  const submitted = useRef<ResponseTemplateRule[] | null>(null);
  const dirty = !!config && (JSON.stringify(drafts) !== JSON.stringify(templateDrafts(config.rules)) || !!reason);
  const state = useRef({dirty, blocked}); state.current = {dirty, blocked};
  const choices = templateModels(models);
  const rule = drafts.find(item => item.id === selected);
  const parsed = parseTemplateRules(drafts);
  useEffect(() => {alive.current = true; return () => {alive.current = false;};}, []);
  useEffect(() => {onDirtyChange(dirty); return () => onDirtyChange(false);}, [dirty, onDirtyChange]);
  useEffect(() => {onBusyChange(busy); return () => onBusyChange(false);}, [busy, onBusyChange]);

  useEffect(() => {previewEpoch.current++; setPreviewResult(''); setPreviewBusy(false);}, [drafts, previewPrompt, previewModel]);
  const preview = async () => {
    if (!parsed.rules || !previewPrompt.trim() || !previewModel) return;
    const epoch = ++previewEpoch.current; setPreviewBusy(true);
    try {
      const r = await adminApi.previewResponseTemplates(parsed.rules, previewPrompt, previewModel);
      if (!alive.current || epoch !== previewEpoch.current) return;
      setPreviewResult((r.winner ? `最终命中：${r.winner}` : '不命中模板，继续正常上游服务') + '\n' + r.matches.map(m => `${m.name}: ${m.result.matched ? '匹配' : '不匹配'} / ${m.result.reason}${m.result.missing_groups.length ? ' / 缺少词组 ' + m.result.missing_groups.join(', ') : ''}`).join('\n'));
    } catch (e) { if (alive.current && epoch === previewEpoch.current) setPreviewResult('预览失败：' + message(e)); }
    finally { if (alive.current && epoch === previewEpoch.current) setPreviewBusy(false); }
  };
  const adopt = (next: ResponseTemplateConfig) => {
    const nextDrafts = templateDrafts(next.rules);
    setConfig(next); setDrafts(nextDrafts); setSelected(previous => nextDrafts.some(item => item.id === previous) ? previous : nextDrafts[0]?.id ?? null);
    setReason(''); setLatest(null); setBlocked(''); setError(''); submitted.current = null;
  };
  const read = async () => {
    if (reading.current || submitting.current) return;
    reading.current = true; setBusy(true); onBusyChange(true);
    try {
      const result = await adminApi.getResponseTemplates();
      if (!result.success || !result.config || typeof result.config.revision !== 'string' || !result.config.revision || !Array.isArray(result.config.rules) || !Array.isArray(result.config.audit)) throw new Error('服务器没有返回有效的模板配置版本');
      templateDrafts(result.config.rules);
      if (!alive.current) return;
      if (state.current.blocked === 'uncertain' && submitted.current && sameTemplateRules(result.config.rules, submitted.current)) {
        adopt(result.config); toast.success('已核对：服务器已保存这次发布');
      } else if (state.current.dirty || state.current.blocked) {
        setLatest(result.config); setError('');
      } else adopt(result.config);
    } catch (failure) {
      if (alive.current) setError(`读取响应模板失败：${message(failure)}。草稿未改动，请重试读取；不会用空规则覆盖服务器配置。`);
    } finally {reading.current = false; if (alive.current) setBusy(false);}
  };
  useEffect(() => {void read();}, [refreshEpoch]);

  const updateRule = (change: Partial<TemplateRuleDraft>) => setDrafts(items => items.map(item => item.id === selected ? {...item, ...change} : item));
  const updateVariant = (index: number, change: Partial<TemplateVariantDraft>) => {
    if (rule) updateRule({variants: rule.variants.map((variant, i) => i === index ? {...variant, ...change} : variant)});
  };
  const add = (example = false) => {
    const next = example ? pelicanTemplateRule() : newTemplateRule();
    setDrafts(items => [...items, next]); setSelected(next.id); setError('');
  };
  const move = (index: number, delta: number) => {
    const next = [...drafts]; [next[index], next[index + delta]] = [next[index + delta], next[index]]; setDrafts(next);
  };
  const removeRule = async () => {
    if (!rule || !(await confirmAction({title: `删除规则「${rule.name || '未命名'}」？`, consequence: '删除只修改草稿，发布后才生效。', confirmLabel: '删除规则', danger: true})) || !alive.current) return;
    const remaining = drafts.filter(item => item.id !== rule.id); setDrafts(remaining); setSelected(remaining[0]?.id ?? null);
  };
  const removeVariant = async (index: number) => {
    if (!rule || !(await confirmAction({title: '删除这个模型变体？', facts: [rule.variants[index].model_id || '尚未选择模型'], consequence: '此变体的代码和价格会从草稿移除，发布后生效。', confirmLabel: '删除变体', danger: true})) || !alive.current) return;
    updateRule({variants: rule.variants.filter((_, i) => i !== index)});
  };
  const discard = async () => {
    if (config && await confirmAction({title: '放弃未发布的修改？', consequence: '恢复最近已读取的服务器配置，当前草稿和发布原因会丢失。', confirmLabel: '放弃修改', danger: true}) && alive.current) adopt(latest ?? config);
  };
  const rebase = async () => {
    if (!latest || !(await confirmAction({title: '保留草稿并采用已读取版本？', consequence: '不会自动合并。下次发布将用整个草稿替换此版本的规则；请先核对他人的修改。', facts: [`版本：${latest.revision}`, `服务器 ${latest.rules.length} 条规则；草稿 ${drafts.length} 条规则`], confirmLabel: '采用版本'})) || !alive.current) return;
    setConfig(latest); setLatest(null); setBlocked(''); setError(''); submitted.current = null;
  };
  const publish = async () => {
    if (!config || busy || blocked || latest || submitting.current || guards.writing.current) return;
    if (!parsed.rules) {setError(parsed.error); return;}
    if (!reason.trim()) {setError('请填写发布原因。'); return;}
    if (new TextEncoder().encode(reason.trim()).length > 1024 || /[\x00-\x1f\x7f]/.test(reason.trim())) {setError('发布原因不能超过 1024 字节或包含换行、控制字符。'); return;}
    const priced = parsed.rules.flatMap(item => item.variants).filter(item => item.price_microcredits > 0);
    if (!(await confirmAction({title: '发布响应模板？', facts: [`按列表顺序发布 ${drafts.length} 条规则，其中 ${drafts.filter(item => item.enabled).length} 条启用。`, `收费变体 ${priced.length} 个，其余变体为 0 credits。`, `原因：${reason.trim()}`],
      consequence: '命中后提供固定模板服务，不是上游模型生成，不产生或虚构上游 tokens。每条生成文件指令下发时收费一次，成功或失败回执均免费；实际写入需要客户端声明兼容文件工具。', confirmLabel: '确认发布'})) || !alive.current) return;
    if (submitting.current || guards.writing.current) return;
    submitting.current = true; guards.writing.current = true; setBusy(true); onBusyChange(true); setError(''); submitted.current = parsed.rules;
    try {
      const result = await adminApi.publishResponseTemplates({expected_revision: config.revision, reason: reason.trim(), rules: parsed.rules});
      if (!result.success || !result.config || typeof result.config.revision !== 'string' || !result.config.revision || !Array.isArray(result.config.audit)) throw new Error('服务器未确认发布版本');
      if (alive.current) {adopt(result.config); toast.success('响应模板已发布');}
    } catch (failure) {
      if (!alive.current) return;
      if (failure instanceof AdminApiError && failure.status === 409) {
        setBlocked('conflict'); setError('版本冲突或配置被拒绝：服务器未接受发布。草稿已保留，请读取服务器版本、核对差异后再决定；不会自动覆盖或重试。' + message(failure));
      } else if (failure instanceof AdminApiError && failure.status >= 400 && failure.status < 500) {
        setError(`发布被拒绝：${message(failure)}。草稿和发布原因已保留。`);
      } else {
        setBlocked('uncertain'); setError(`发布结果未确认：${message(failure)}。可能已经生效，请先读取服务器版本核对，不要重复发布。草稿已保留。`);
      }
    } finally {guards.writing.current = false; submitting.current = false; if (alive.current) setBusy(false);}
  };

  return <div className="page-stack response-templates">
    <TopbarActions><button type="button" className="btn btn-primary" disabled={!config || busy || !dirty || !!blocked || !!latest} onClick={() => void publish()}>发布模板</button></TopbarActions>
    <section className="panel template-intro" aria-label="响应模板说明">
      <h3>响应模板</h3>
      <p>这是固定模板服务，不是上游模型生成；不调用上游，不消耗或虚构上游 tokens。模型变体决定返回的文件代码、消息与固定服务费。</p>
      <p>固定服务费按每条生成文件指令在下发时收取一次；成功或失败的工具结果回执均免费，不重复收费。新变体默认 <strong>0 credits（免费）</strong>，不会自动套用模型的 token 单价。</p>
      <p>实际写出文件需要客户端声明兼容工具：<code>fsWrite(path,text)</code>、<code>Write(file_path,content)</code>、<code>write_file(path,content)</code> 或 <code>writeFile(path,content)</code>。下发指令不代表文件已写入成功。</p>
      <p>未匹配到模型变体时，继续正常模型服务。后端默认没有任何规则，示例只加入本地草稿，不自动启用或发布。</p>
      <p className="muted">从上到下为规则优先级；完整匹配、包含匹配或保守意图匹配。最多 32 条规则，每条最多 32 个不同模型变体；每份代码 ≤256 KiB，总配置 ≤2 MiB。</p>
      <div className="button-row"><button type="button" className="btn" disabled={busy} onClick={() => void read()}>{busy ? '处理中…' : '读取服务器版本'}</button>
        <span className="muted">{config ? `当前基准版本：${config.revision}${dirty ? ' · 有未发布的修改' : ''}` : '尚未读取配置，禁止发布'}</span></div>
    </section>
    {error && <p role="alert" className="message message-error">{error}</p>}
    {modelsFailed && <p role="alert" className="message message-warning">商业模型列表读取失败。已加载的选择仅供核对；请用顶部刷新重试，不会自行编造模型 ID。</p>}
    {latest && <section className="panel template-intro" aria-label="服务器版本核对">
      <h3>服务器版本已读取，草稿仍保留</h3><p>服务器版本：{latest.revision}。请核对以下完整规则；这里只展示文本，不执行 HTML。</p>
      <details><summary>查看服务器规则（只读）</summary><pre className="template-server-code">{JSON.stringify(latest.rules, null, 2)}</pre></details>
      <div className="button-row"><button type="button" className="btn" disabled={busy} onClick={() => void rebase()}>保留草稿并采用此版本</button>
        <button type="button" className="btn" disabled={busy} onClick={() => void discard()}>放弃草稿，使用服务器版本</button></div>
    </section>}
    {config && <>
      <fieldset disabled={busy || !!blocked || !!latest} className="template-layout">
        <section className="panel template-list" aria-label="模板规则列表">
          <div className="panel-head"><h3>规则优先级</h3><span className="muted">{drafts.length} / 32</span></div>
          {!drafts.length && <p className="template-empty">尚无规则，不拦截任何请求。新增规则默认停用，需要手动启用并发布。</p>}
          <ol>{drafts.map((item, index) => <li key={item.id}>
            <button type="button" className="btn template-rule-select" aria-pressed={selected === item.id} onClick={() => setSelected(item.id)}>{index + 1}. {item.name || '未命名规则'} · {item.enabled ? '启用' : '停用'}</button>
            <div className="button-row"><button type="button" className="btn-text" aria-label={`上移规则 ${index + 1}`} disabled={index === 0} onClick={() => move(index, -1)}>上移</button>
              <button type="button" className="btn-text" aria-label={`下移规则 ${index + 1}`} disabled={index === drafts.length - 1} onClick={() => move(index, 1)}>下移</button></div>
          </li>)}</ol>
          <div className="button-row"><button type="button" className="btn" disabled={drafts.length >= MAX_TEMPLATE_ITEMS} onClick={() => add()}>新增规则</button>
            <button type="button" className="btn-text" disabled={drafts.length >= MAX_TEMPLATE_ITEMS} onClick={() => add(true)}>添加鹈鹕示例（停用、免费）</button></div>
        </section>
        {rule && <section className="panel template-editor" aria-label="规则编辑器">
          <div className="panel-head"><h3>编辑规则</h3><Switch checked={rule.enabled} label="启用规则" onChange={enabled => updateRule({enabled})}/>
            <button type="button" className="btn-text" onClick={() => void removeRule()}>删除规则</button></div>
          <div className="form-grid form-grid-2">
            <label className="field"><span className="field-label">规则名称</span><input value={rule.name} onChange={event => updateRule({name: event.target.value})}/></label>
            <label className="field"><span className="field-label">匹配方式</span><select value={rule.match_mode} onChange={event => updateRule({match_mode: event.target.value as TemplateRuleDraft['match_mode'], ...(event.target.value === 'intent' ? {intent: rule.intent ?? pelicanIntent()} : {})})}><option value="exact">完整匹配</option><option value="contains">包含匹配</option><option value="intent">意图匹配（同义词组）</option></select></label>
            <label className="field field-span"><span className="field-label">匹配文本</span><textarea value={rule.match_text} onChange={event => updateRule({match_text: event.target.value})}/></label>
          </div>
          {rule.match_mode === 'intent' && <section className="panel template-intro">
            <p>每行是一组必须出现的概念；同组同义词用 | 分隔。主体与动作应放在同一组短语中（如“鹈鹕骑”“骑自行车的鹈鹕”），不要拆成互不关联的关键词。缺少概念、否定、引用或修改类请求保守交给上游。不会自动猜测错别字，也不能保证任意语义零误判。</p>
            <label className="field"><span>必须概念（组间 AND、组内 OR）</span><textarea rows={8} value={(rule.intent?.groups ?? []).map(g => g.join('|')).join('\n')} onChange={e => updateRule({intent: {groups: e.target.value.split('\n').map(line => line.split('|')), exclude: rule.intent?.exclude ?? []}})}/></label>
            <label className="field"><span>排除词（每行一个）</span><textarea value={(rule.intent?.exclude ?? []).join('\n')} onChange={e => updateRule({intent: {groups: rule.intent?.groups ?? [], exclude: e.target.value ? e.target.value.split('\n') : []}})}/></label>
          </section>}
          {rule.variants.map((variant, index) => <section className="template-variant" aria-label={`模型变体 ${index + 1}`} key={`${rule.id}-${index}`}>
            <div className="panel-head"><h4>模型变体 {index + 1}</h4><button type="button" className="btn-text" onClick={() => void removeVariant(index)}>删除变体</button></div>
            <div className="form-grid form-grid-2">
              <label className="field"><span className="field-label">商业模型</span><select value={variant.model_id} onChange={event => updateVariant(index, {model_id: event.target.value})}>
                <option value="">请选择已配置模型</option>
                {variant.model_id && !choices.some(model => model.id === variant.model_id) && <option value={variant.model_id}>{variant.model_id}（配置中已有，当前模型目录未找到）</option>}
                {choices.map(model => <option key={model.id} value={model.id} disabled={rule.variants.some((other, i) => i !== index && other.model_id === model.id)}>{model.label}</option>)}
              </select><span className="field-hint">使用客户端可见的模型 ID；同一规则内不可重复。未列出的旧 ID 会保留，不自动替换。</span></label>
              <label className="field"><span className="field-label">相对 HTML 文件路径</span><input value={variant.file_path} placeholder="pages/demo.html" onChange={event => updateVariant(index, {file_path: event.target.value})}/></label>
              <label className="field"><span className="field-label">固定服务费（credits / 生成文件指令）</span><input type="text" inputMode="decimal" value={variant.price_credits} onChange={event => updateVariant(index, {price_credits: event.target.value})}/>
                <span className="field-hint">0–1000，最多 6 位小数；{templatePrice(variant.price_credits) === null ? '价格格式无效' : `${templatePrice(variant.price_credits)} microcredits${templatePrice(variant.price_credits) === 0 ? ' · 免费' : ' · 收费'}`}。成功或失败回执均免费。</span></label>
              <label className="field field-span"><span className="field-label">完整 HTML 代码</span><textarea className="template-code" rows={16} spellCheck={false} value={variant.content} onChange={event => updateVariant(index, {content: event.target.value})}/>
                <span className="field-hint">完整代码可编辑，只作为文本保存；管理后台不会渲染或执行。UTF-8：{new TextEncoder().encode(variant.content).length} / 262144 字节。</span></label>
              {!variant.delivery && <label className="field"><span className="field-label">响应延迟（毫秒，0–30000）</span><input type="number" min="0" max="30000" step="1" value={variant.delay_ms ?? 0} onChange={event => updateVariant(index, {delay_ms: Number(event.target.value)})}/></label>}
              {!variant.delivery && <label className="field"><span className="field-label">前置消息</span><textarea value={variant.preamble} onChange={event => updateVariant(index, {preamble: event.target.value})}/></label>}
              {!variant.delivery && <label className="field"><span className="field-label">完成消息</span><textarea value={variant.completion} onChange={event => updateVariant(index, {completion: event.target.value})}/></label>}
            </div>
            <TemplateDeliveryEditor value={variant.delivery} onChange={delivery => updateVariant(index, {delivery})}/>
          </section>)}
          <button type="button" className="btn" disabled={rule.variants.length >= MAX_TEMPLATE_ITEMS || !choices.some(model => !rule.variants.some(variant => variant.model_id === model.id))} onClick={() => updateRule({variants: [...rule.variants, newTemplateVariant()]})}>新增模型变体</button>
          {!choices.length && <p className="muted">暂无可选商业模型，请先在“模型与定价”配置模型，再刷新。</p>}
        </section>}
      </fieldset>
      <section className="panel template-intro" aria-label="匹配预览">
        <h3>草稿命中预览（不扣费、不调用上游）</h3>
        <p className="muted">使用服务器实际匹配器；停用规则不参与命中。预览仅判断文本和模型，实际发送还会检查工具、卡状态和余额。</p>
        <label className="field"><span>请求模型</span><select aria-label="请求模型" value={previewModel} onChange={e => setPreviewModel(e.target.value)}><option value="">选择模型</option>{choices.map(c => <option key={c.id} value={c.id}>{c.label}</option>)}</select></label>
        <label className="field"><span>用户请求</span><textarea value={previewPrompt} onChange={e => setPreviewPrompt(e.target.value)}/></label>
        <button type="button" className="btn" disabled={previewBusy || !parsed.rules || !previewModel || !previewPrompt.trim()} onClick={() => void preview()}>{previewBusy ? '预览中…' : '预览命中结果'}</button>
        <pre role="status" style={{whiteSpace: 'pre-wrap'}}>{previewResult}</pre>
      </section>
      <section className="panel template-intro" aria-label="发布设置">
        <label className="field"><span className="field-label">发布原因（必填）</span><textarea maxLength={500} disabled={busy || !!blocked || !!latest} value={reason} onChange={event => setReason(event.target.value)} placeholder="说明本次匹配规则、代码或固定服务费的变更原因"/></label>
        {parsed.error && dirty && <p className="field-error" role="status">{parsed.error}</p>}
        <div className="editor-actions"><button type="button" className="btn" disabled={busy || !dirty || (!!blocked && !latest)} onClick={() => void discard()}>放弃修改</button></div>
      </section>
      <section className="panel template-intro" aria-label="模板发布审计"><details><summary>发布审计（{config.audit.length}）</summary><pre className="template-server-code">{JSON.stringify(config.audit, null, 2)}</pre></details></section>
    </>}
  </div>;
}

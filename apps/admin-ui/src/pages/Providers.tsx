// 供应商与 Key: each provider with an on/off switch, its name, address and format (编辑供应商) and
// its keys, each Key's live health and last error with the action that fixes it: a Key refused as
// invalid gets a new secret (更换密钥), a cooling one is put back to work (解除冷却). The key
// editor opens only when a key is being edited or added.
import {useEffect, useRef, useState, type MutableRefObject} from 'react';
import {adminApi} from '../api';
import {ask, confirmAction} from '../components/confirm';
import {Menu} from '../components/menu';
import {Modal} from '../components/modal';
import {toast} from '../components/toast';
import {ListState, StatusBadge, Switch, Tag, TopbarActions} from '../components/ui';
import {formatFullDateTime, formatRelative} from '../format';
import ProviderKeyEditor from '../ProviderKeyEditor';
import {explainRefusal, isRefusal, type PublishOutcome} from '../refusal';
import {isLive, lossFacts, modelName, nameList, routeLosses, targetsOf} from '../routes';
import {credentialFailure, failureLabel, keyAlert, keyStatusView, providerFormatLabel} from '../status';
import type {Intent, Refresh, ReportError, ReportRoute, Row, WriteGuards} from '../types';

/**
 * Which key the editor shows: none, a new provider, a new key of a provider, or an existing key
 * (from an address, first only its ID: the provider is found once the Keys are loaded).
 */
export interface KeyEditing {providerId?: string; keyId?: string; suggestedKeyId?: string}

const errorText = (error: unknown) => (error instanceof Error ? error.message : String(error));
const validText = (text: string, max: number) => !!text.trim() && new TextEncoder().encode(text).length <= max && !/[\x00-\x1f\x7f-\x9f]/.test(text);
/** A provider's API format as the update endpoint takes it (older data says `openai` in api_type). */
const formatOf = (provider: Row): 'open_ai' | 'anthropic' => ['open_ai', 'openai'].includes(String(provider.format ?? provider.api_type)) ? 'open_ai' : 'anthropic';
const FORMAT_NAME = {open_ai: 'OpenAI', anthropic: 'Anthropic'};

/** 编辑供应商: its name, upstream address and API format; its ID and Keys stay as they are. */
function ProviderEditor({provider, routed, onClose, onSave}: {
  provider: Row;
  /** The models that route through it (primary or backup), whose requests follow the address and format. */
  routed: string[];
  onClose: () => void;
  onSave: (edit: {name?: string; base_url?: string; format?: 'open_ai' | 'anthropic'}) => Promise<string | null>;
}) {
  const id = String(provider.id);
  const [name, setName] = useState(String(provider.name || id));
  const [baseUrl, setBaseUrl] = useState(String(provider.base_url ?? ''));
  const [format, setFormat] = useState(formatOf(provider));
  const [error, setError] = useState('');
  const [saving, setSaving] = useState(false);
  const save = async () => {
    const edit: {name?: string; base_url?: string; format?: 'open_ai' | 'anthropic'} = {};
    if (name.trim() !== String(provider.name || id)) edit.name = name.trim();
    if (baseUrl.trim() !== String(provider.base_url ?? '')) edit.base_url = baseUrl.trim();
    if (format !== formatOf(provider)) edit.format = format;
    if (!Object.keys(edit).length) {setError('没有要保存的修改'); return;}
    if (edit.name !== undefined && !validText(edit.name, 256)) {setError('名称不能为空，不超过 256 字节，不含控制字符'); return;}
    if (edit.base_url !== undefined) {
      let url: URL;
      try {url = new URL(edit.base_url);} catch {setError('请填写完整的上游地址，例如 https://api.example.com'); return;}
      if (!(url.protocol === 'https:' || (url.protocol === 'http:' && ['localhost', '127.0.0.1'].includes(url.hostname)))) {setError('上游地址须使用 HTTPS（本机 localhost、127.0.0.1 可用 HTTP）'); return;}
    }
    const moves = edit.base_url !== undefined || edit.format !== undefined;
    const confirmed = await confirmAction({
      title: `保存供应商 ${String(provider.name || id)}？`,
      facts: [...(edit.name !== undefined ? [`名称：${String(provider.name || id)} → ${edit.name}`] : []),
        ...(edit.base_url !== undefined ? [`上游地址：${String(provider.base_url || '未配置')} → ${edit.base_url}`] : []),
        ...(edit.format ? [`接口格式：${FORMAT_NAME[formatOf(provider)]} → ${FORMAT_NAME[edit.format]}`] : []),
        ...(moves && routed.length ? [`走这个供应商的模型：${nameList(routed)}`] : [])],
      consequence: moves ? '保存后，发到这个供应商的请求马上改用新的地址和格式；填错的话这些请求会失败。保存后可以在它的 Key 里点“测试”核对。' : '只改显示的名称，线路和请求不受影响。',
      confirmLabel: '保存',
    });
    if (!confirmed) return;
    setSaving(true); setError('');
    const failure = await onSave(edit);
    setSaving(false);
    if (failure) setError(failure);
  };
  return <Modal label="编辑供应商" onClose={onClose} busy={saving} className="dialog-form">
    <h3 className="modal-title">编辑供应商 · <span className="mono">{id}</span></h3>
    {error && <p role="alert" className="form-error">{error}</p>}
    <fieldset disabled={saving} className="form-grid">
      <label className="field"><span className="field-label">名称</span>
        <input aria-label="供应商名称" maxLength={256} value={name} onChange={event => setName(event.target.value)}/></label>
      <label className="field"><span className="field-label">上游地址</span>
        <input aria-label="上游地址" type="url" placeholder="https://api.example.com" value={baseUrl} onChange={event => setBaseUrl(event.target.value)}/></label>
      <label className="field"><span className="field-label">接口格式</span>
        <select aria-label="接口格式" value={format} onChange={event => setFormat(event.target.value as 'open_ai' | 'anthropic')}>
          <option value="anthropic">Anthropic</option><option value="open_ai">OpenAI</option>
        </select></label>
    </fieldset>
    <p className="muted">供应商 ID 和它的 Key 不变。</p>
    <div className="modal-actions">
      <button type="button" className="btn" disabled={saving} onClick={onClose}>取消</button>
      <button type="button" className="btn btn-primary" disabled={saving} onClick={() => void save()}>{saving ? '保存中…' : '保存'}</button>
    </div>
  </Modal>;
}

function ModelTags({models}: {models: unknown}) {
  if (!Array.isArray(models)) return <Tag tone="warning" title="旧版 Key 没有限制模型，建议改成明确的模型列表">全部模型（旧版）</Tag>;
  if (!models.length) return <Tag tone="danger" title="这个 Key 不会被使用">未授权模型</Tag>;
  const names = models.map(String);
  return <span className="model-tags" title={names.join('\n')}>
    {names.slice(0, 3).map(name => <Tag key={name}>{name}</Tag>)}
    {names.length > 3 && <span className="muted">+{names.length - 3}</span>}
  </span>;
}

export default function ProvidersPage({providers, providerKeys, models, groups = [], loading, failed, refresh, guards, reportError, editing, setEditing, providerDirty, editorBusy, onDirtyChange, onBusyChange, mergeKey, onListModel, onHideModels, intent, intentRevision = 0, onRoute}: {
  providers: Row[];
  providerKeys: Row[];
  models: Row[];
  groups?: Row[];
  loading: boolean;
  failed: boolean;
  refresh: Refresh;
  guards: WriteGuards;
  reportError: ReportError;
  editing: KeyEditing | null;
  setEditing: (editing: KeyEditing | null) => void;
  providerDirty: MutableRefObject<boolean>;
  editorBusy: MutableRefObject<boolean>;
  onDirtyChange: (dirty: boolean) => void;
  onBusyChange: (busy: boolean) => void;
  mergeKey: (saved: Row) => void;
  /** Opens 模型与定价 › 上架模型 for this provider's upstream model. */
  onListModel?: (providerId: string, model: string) => void;
  /** 同时隐藏这些模型: publishes these models hidden, before the change that leaves them without a route. */
  onHideModels?: (models: Row[], reason: string) => Promise<PublishOutcome>;
  intent?: Intent['providers'];
  /** Changes when Back or Forward brings the page to another `intent`. */
  intentRevision?: number;
  onRoute?: ReportRoute;
}) {
  const {writing} = guards;
  const [switching, setSwitching] = useState<string | null>(null);
  const [resetting, setResetting] = useState<string | null>(null);
  const [scrollToken, setScrollToken] = useState(0);
  // 更换密钥: the editor opens with its API Key field focused.
  const [secretToken, setSecretToken] = useState(0);
  const [editingProvider, setEditingProvider] = useState<Row | null>(null);
  const nowSecs = Date.now() / 1000;
  useEffect(() => {if (scrollToken) document.getElementById('key-editor')?.scrollIntoView({behavior: 'smooth', block: 'start'});}, [scrollToken]);
  const providerName = (id: unknown) => String(providers.find(provider => provider.id === id)?.name || id);
  // The models routed through a provider (primary or backup), in any state.
  const routedBy = (id: string) => models.filter(model => targetsOf(model).some(target => target.provider_id === id));

  // A link names a provider or a Key (运营概览's 需要关注): it is scrolled into view and marked
  // until the operator turns to something else.
  const [pointed, setPointed] = useState<{provider?: string; key?: string}>({provider: intent?.provider, key: intent?.key});
  const appliedRevision = useRef(intentRevision);
  useEffect(() => {
    if (intentRevision === appliedRevision.current) return;
    appliedRevision.current = intentRevision;
    setPointed({provider: intent?.provider, key: intent?.key});
    setEditing(intent?.edit ? {keyId: intent.edit} : null);
  // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [intentRevision]);
  const scrolledTo = useRef('');
  useEffect(() => {
    const target = pointed.key ? `[data-key-id="${CSS.escape(pointed.key)}"]` : pointed.provider ? `[data-provider-id="${CSS.escape(pointed.provider)}"]` : '';
    const element = target ? document.querySelector(target) : null;
    if (!element || scrolledTo.current === target) return;
    scrolledTo.current = target;
    element.scrollIntoView({block: 'center'});
  }, [pointed, providers, providerKeys]);
  // An editor opened from an address knows only the Key's ID; its provider comes with the Keys.
  useEffect(() => {
    if (!editing?.keyId || editing.providerId) return;
    const key = providerKeys.find(item => item.id === editing.keyId);
    if (key) setEditing({providerId: String(key.provider_id), keyId: editing.keyId});
    else if (!loading && !failed) setEditing(null);
  }, [editing, providerKeys, loading, failed, setEditing]);
  useEffect(() => {onRoute?.({providers: {...pointed, edit: editing?.keyId}});}, [onRoute, pointed, editing]);

  const edit = async (target: KeyEditing | null, focusSecret = false) => {
    const same = !!target && !!editing && target.providerId === editing.providerId && target.keyId === editing.keyId;
    if (!same) {
      if (editorBusy.current) {toast.info('正在保存，请稍候'); return;}
      if (providerDirty.current && !(await confirmAction({title: '有未保存的修改，确定离开？', consequence: 'Key 的修改还没有保存。', confirmLabel: '放弃修改'}))) return;
      providerDirty.current = false;
      setEditing(target);
      setPointed({});
    }
    if (target) setScrollToken(token => token + 1);
    if (target && focusSecret) setSecretToken(token => token + 1);
  };

  // 编辑供应商: only the fields changed are sent; a refusal is explained and the dialog stays open.
  const saveProvider = async (provider: Row, change: {name?: string; base_url?: string; format?: 'open_ai' | 'anthropic'}): Promise<string | null> => {
    if (writing.current) return '另一个操作还没完成，请稍候';
    writing.current = true; reportError('');
    const name = String(provider.name || provider.id);
    try {
      const response = await adminApi.updateProvider({id: String(provider.id), ...change});
      if (response.success !== true) throw new Error('服务端未确认保存');
      toast.success(`已保存供应商 ${change.name ?? name}`);
      setEditingProvider(null);
      await refresh();
      return null;
    } catch (error) {
      if (isRefusal(error)) return `没有保存：${explainRefusal(errorText(error))}`;
      setEditingProvider(null);
      reportError(`没收到保存结果（${errorText(error)}）。请刷新核对 ${name} 的名称、地址和格式后再操作。`);
      return null;
    } finally {writing.current = false;}
  };

  // 删除供应商: the server refuses while a model routes through it or it has Keys; the dialog says
  // what still uses it, and a refusal is explained.
  const removeProvider = async (provider: Row) => {
    const id = String(provider.id), name = String(provider.name || id);
    if (writing.current || loading || failed) return;
    const routed = routedBy(id).map(model => modelName(model, models, groups)), keys = providerKeys.filter(key => key.provider_id === id).map(key => String(key.id));
    const blocked = routed.length > 0 || keys.length > 0;
    const confirmed = await confirmAction({
      title: `删除供应商 ${name}？`,
      facts: [`${id} · ${String(provider.base_url || '地址未配置')}`, ...(routed.length ? [`还有模型的线路用它：${nameList(routed)}`] : []), ...(keys.length ? [`它还有 Key：${nameList(keys)}`] : [])],
      consequence: blocked ? `服务器会拒绝删除：先${routed.length ? '在“模型与定价”把这些模型换到别的供应商（主线路和备用线路都算）或删除它们，' : ''}${keys.length ? '删除它的 Key，' : ''}然后再删除供应商。`
        : '删除后不能恢复：要再用，需重新添加供应商和 Key。',
      confirmLabel: '删除', danger: true,
    });
    if (!confirmed || writing.current) return;
    writing.current = true; reportError('');
    try {
      const response = await adminApi.deleteProvider(id);
      if (response.success !== true) throw new Error('服务端未确认删除');
      toast.success(`已删除供应商 ${name}`);
      if (editing?.providerId === id) {providerDirty.current = false; setEditing(null);}
      await refresh();
    } catch (error) {
      reportError(isRefusal(error) ? `没能删除 ${name}：${explainRefusal(errorText(error))}` : `删除结果未确认：${errorText(error)}。请刷新核对后再操作。`);
    } finally {writing.current = false;}
  };

  const toggle = async (provider: Row, enable: boolean) => {
    const id = String(provider.id);
    if (writing.current || loading || failed || switching) return;
    const name = String(provider.name || id);
    const routed = models.filter(model => model.target_provider_id === id).map(model => String(model.exposed_model_id ?? model.id));
    const losses = routeLosses(models, {providers, keys: providerKeys}, {providers: providers.map(item => item.id === id ? {...item, enabled: false} : item), keys: providerKeys});
    const facts = lossFacts(losses, model => modelName(model, models, groups));
    const hideable = !!onHideModels && losses.down.length > 0;
    const answer = await ask(enable ? {
      title: `启用 ${name}？`,
      consequence: routed.length ? `${routed.length} 个模型会重新使用这个供应商。` : '没有模型使用这个供应商。',
      confirmLabel: '启用',
    } : {
      title: `停用 ${name}？`,
      facts: facts.length ? facts : routed.length ? [`路由到这里的模型：${nameList(routed)}（都没有在售）`] : undefined,
      option: hideable ? {label: `同时隐藏将无可用线路的 ${losses.down.length} 个模型（先隐藏，再停用）`} : undefined,
      consequence: losses.down.length ? `不隐藏的话，这 ${losses.down.length} 个模型仍在客户的模型列表里，但请求会失败。`
        : losses.takeover.length ? `${losses.takeover.length} 个模型会改由备用线路服务，其余不受影响。` : routed.length ? '在售模型不受影响。' : '没有模型使用这个供应商。',
      confirmLabel: '停用',
      danger: losses.down.length > 0,
    });
    if (!answer.confirmed || writing.current) return;
    writing.current = true; setSwitching(id); reportError('');
    let hidden = '';
    try {
      if (answer.option && onHideModels) {
        const names = nameList(losses.down.map(model => modelName(model, models, groups)));
        const outcome = await onHideModels(losses.down, `停用 ${name} 前隐藏将无可用线路的模型：${names}`);
        if (!outcome.ok) {
          reportError(outcome.uncertain ? `没收到隐藏结果（${outcome.message}），${name} 没有停用。请点“刷新”核对这些模型是否已隐藏，再重新操作，不要重复提交。`
            : `没能隐藏这些模型：${outcome.message}。${name} 没有停用。`);
          return;
        }
        hidden = names;
      }
      const response = await adminApi.updateProviderStatus(id, enable);
      if (!response.success) throw new Error('服务端未确认状态变更');
      toast.success(hidden ? `已隐藏 ${hidden}，并停用 ${name}` : `已${enable ? '启用' : '停用'} ${name}`);
      await refresh();
    } catch (error) {
      reportError(`${hidden ? `已隐藏 ${hidden}；但` : ''}切换结果未确认：${errorText(error)}。请刷新核对供应商状态后再操作。`);
      if (hidden) void refresh();
    } finally {writing.current = false; setSwitching(null);}
  };

  // 解除冷却: ends a Key's cooldown; the server puts it back into rotation at once.
  const reset = async (key: Row) => {
    const id = String(key.id), providerId = String(key.provider_id);
    if (writing.current || loading || failed || resetting) return;
    const confirmed = await confirmAction({
      title: `解除 Key ${id} 的冷却？`,
      facts: [`现在：${keyStatusView(key, Date.now() / 1000).label}`, ...(failureLabel(key.last_error) ? [`最近错误：${failureLabel(key.last_error)}`] : [])],
      consequence: '这个 Key 马上重新接请求；上游仍有问题的话，它会再次进入冷却。',
      confirmLabel: '解除冷却',
    });
    if (!confirmed || writing.current) return;
    writing.current = true; setResetting(id); reportError('');
    try {
      const response = await adminApi.resetKey(providerId, id);
      if (!response.success) throw new Error('服务端未确认解除冷却');
      toast.success(`已解除 Key ${id} 的冷却`);
      await refresh();
    } catch (error) {
      reportError(isRefusal(error) ? `没能解除 Key ${id} 的冷却：${explainRefusal(errorText(error))}` : `解除冷却的结果未确认：${errorText(error)}。请刷新核对 Key 状态后再操作。`);
    } finally {writing.current = false; setResetting(null);}
  };

  const nextKeyId = (providerId: string) => {
    const taken = new Set(providerKeys.filter(key => key.provider_id === providerId).map(key => String(key.id)));
    let index = 1;
    while (taken.has(`${providerId}-key-${index}`)) index++;
    return `${providerId}-key-${index}`;
  };
  const selectedKey = editing?.keyId ? providerKeys.find(key => key.id === editing.keyId && key.provider_id === editing.providerId) : undefined;

  return <div className="page-stack">
    <TopbarActions><button type="button" className="btn btn-primary" onClick={() => void edit({})}>＋ 添加供应商</button></TopbarActions>
    {providers.map(provider => {
      const id = String(provider.id);
      const keys = providerKeys.filter(key => key.provider_id === id);
      const enabled = provider.enabled !== false;
      return <section key={id} className={`panel provider-card${pointed.provider === id ? ' is-pointed' : ''}`} data-provider-id={id}>
        <div className="provider-head">
          <div className="provider-name">
            <h3>{String(provider.name || id)}</h3>
            <span className="mono muted">{String(provider.base_url || '地址未配置')}</span>
            {providerFormatLabel(provider) && <Tag>{providerFormatLabel(provider)}</Tag>}
          </div>
          <span className="provider-tools">
            <Switch checked={enabled} label={`启用 ${String(provider.name || id)}`} busy={switching === id}
              disabled={!!switching || loading || failed} title={failed ? '供应商列表没有刷新成功，暂不能操作' : undefined}
              onChange={next => void toggle(provider, next)}/>
            <Menu label={`${String(provider.name || id)} 的更多操作`} disabled={loading || failed} title={failed ? '供应商列表没有刷新成功，暂不能操作' : undefined} items={[
              {label: '编辑名称、地址和格式', onSelect: () => setEditingProvider(provider)},
              {label: '删除供应商', danger: true, onSelect: () => void removeProvider(provider)},
            ]}/>
          </span>
        </div>
        {keys.length ? <div className="table-scroll"><table className="table keys-table">
          <thead><tr><th className="col-key">Key</th><th>模型</th><th className="num col-weight">权重</th><th className="col-status">状态</th><th>最近错误</th><th className="col-actions"><span className="sr-only">操作</span></th></tr></thead>
          <tbody>{keys.map(key => {
            const active = editing?.providerId === id && editing?.keyId === key.id;
            return <tr key={String(key.id)} data-key-id={String(key.id)} className={[active && 'is-selected', pointed.key === key.id && 'is-pointed'].filter(Boolean).join(' ') || undefined}>
              <td className="mono nowrap col-key" title={String(key.id)}>{String(key.id)}</td>
              <td><ModelTags models={key.allowed_models}/></td>
              <td className="num">{String(key.weight ?? 1)}</td>
              <td className="col-status"><StatusBadge view={keyStatusView(key, nowSecs)}/></td>
              <td>{failureLabel(key.last_error)
                ? <span className="clip clip-reason key-error" title={`${failureLabel(key.last_error)}${failureLabel(key.last_error) !== key.last_error ? `（${key.last_error}）` : ''}${typeof key.last_error_at === 'number' ? `\n${formatFullDateTime(key.last_error_at)}` : ''}`}>
                  {typeof key.last_error_at === 'number' && <span className="muted">{formatRelative(key.last_error_at)} · </span>}{failureLabel(key.last_error)}</span>
                : <span className="muted">—</span>}</td>
              <td className="col-actions"><span className="row-actions">
                {/* The action follows the failure: a refused Key needs a new secret; a cooling one can go back to work. */}
                {keyAlert(key, nowSecs) && credentialFailure(key)
                  ? <button type="button" className="btn-text" title="这个 Key 被上游拒绝（无效或没有权限）：填写新的 API Key" onClick={() => void edit({providerId: id, keyId: String(key.id)}, true)}>更换密钥</button>
                  : keyAlert(key, nowSecs) === 'cooldown' && <button type="button" className="btn-text" disabled={!!resetting || loading || failed} title="结束冷却，马上重新接请求"
                    onClick={() => void reset(key)}>{resetting === String(key.id) ? '解除中…' : '解除冷却'}</button>}
                <button type="button" className="btn-text" onClick={() => void edit({providerId: id, keyId: String(key.id)})}>编辑</button>
              </span></td>
            </tr>;
          })}</tbody>
        </table></div> : <p className="muted">还没有 Key</p>}
        <div className="provider-foot"><button type="button" className="btn-text" onClick={() => void edit({providerId: id, suggestedKeyId: nextKeyId(id)})}>＋ 添加 Key</button></div>
      </section>;
    })}
    {!providers.length && <section className="panel"><ListState loading={loading} failed={failed} empty="还没有供应商" onRetry={() => void refresh()}
      action={<button type="button" className="btn btn-small" onClick={() => void edit({})}>添加供应商</button>}/></section>}

    {editing && (editing.providerId || !editing.keyId) && <ProviderKeyEditor key={`${editing.providerId ?? 'new'}:${editing.keyId ?? 'new'}`} selectedKey={selectedKey} preset={editing}
      knownModels={[...new Set(models.flatMap(model => targetsOf(model).filter(target => target.provider_id === editing.providerId).map(target => target.target_model)).filter(Boolean))]}
      onSaleModels={[...new Set(models.filter(isLive).flatMap(model => [String(model.exposed_model_id ?? ''), ...targetsOf(model).map(target => target.target_model)]).filter(Boolean))]}
      providerName={editing.providerId ? providerName(editing.providerId) : undefined} focusSecret={secretToken}
      modelsKnown={models.length > 0} onListModel={onListModel} knownProviders={providers.map(provider => String(provider.id))}
      routes={{models, groups, providers, keys: providerKeys}} onHideModels={onHideModels}
      onDeleted={() => {providerDirty.current = false; setEditing(null); void refresh();}}
      onDirtyChange={onDirtyChange} onBusyChange={onBusyChange} onClose={() => void edit(null)}
      onSaved={saved => {
        if (saved) {mergeKey(saved); setEditing({providerId: String(saved.provider_id), keyId: String(saved.id)});}
        // A new provider was saved: its card (with its first Key) appears after the refresh.
        else {providerDirty.current = false; setEditing(null);}
        void refresh();
      }}/>}
    {editingProvider && <ProviderEditor provider={editingProvider} routed={routedBy(String(editingProvider.id)).filter(isLive).map(model => modelName(model, models, groups))}
      onClose={() => setEditingProvider(null)} onSave={change => saveProvider(editingProvider, change)}/>}
  </div>;
}

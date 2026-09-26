// 供应商与 Key: each provider with an on/off switch and its keys; the key editor opens only
// when a key is being edited or added.
import {useEffect, useState, type MutableRefObject} from 'react';
import {adminApi} from '../api';
import {confirmAction} from '../components/confirm';
import {toast} from '../components/toast';
import {ListState, StatusBadge, Switch, Tag, TopbarActions} from '../components/ui';
import ProviderKeyEditor from '../ProviderKeyEditor';
import {keyStatusView} from '../status';
import type {Refresh, ReportError, Row, WriteGuards} from '../types';

/** Which key the editor shows: none, a new provider, a new key of a provider, or an existing key. */
export interface KeyEditing {providerId?: string; keyId?: string; suggestedKeyId?: string}

const errorText = (error: unknown) => (error instanceof Error ? error.message : String(error));

function ModelTags({models}: {models: unknown}) {
  if (!Array.isArray(models)) return <Tag tone="warning" title="旧版 Key 没有限制模型，建议改成明确的模型列表">全部模型（旧版）</Tag>;
  if (!models.length) return <Tag tone="danger" title="这个 Key 不会被使用">未授权模型</Tag>;
  const names = models.map(String);
  return <span className="model-tags" title={names.join('\n')}>
    {names.slice(0, 3).map(name => <Tag key={name}>{name}</Tag>)}
    {names.length > 3 && <span className="muted">+{names.length - 3}</span>}
  </span>;
}

export default function ProvidersPage({providers, providerKeys, models, loading, failed, refresh, guards, reportError, editing, setEditing, providerDirty, editorBusy, onDirtyChange, onBusyChange, mergeKey}: {
  providers: Row[];
  providerKeys: Row[];
  models: Row[];
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
}) {
  const {writing} = guards;
  const [switching, setSwitching] = useState<string | null>(null);
  const [scrollToken, setScrollToken] = useState(0);
  const nowSecs = Date.now() / 1000;
  useEffect(() => {if (scrollToken) document.getElementById('key-editor')?.scrollIntoView({behavior: 'smooth', block: 'start'});}, [scrollToken]);

  const edit = async (target: KeyEditing | null) => {
    const same = !!target && !!editing && target.providerId === editing.providerId && target.keyId === editing.keyId;
    if (!same) {
      if (editorBusy.current) {toast.info('正在保存，请稍候'); return;}
      if (providerDirty.current && !(await confirmAction({title: '有未保存的修改，确定离开？', consequence: 'Key 的修改还没有保存。', confirmLabel: '放弃修改'}))) return;
      providerDirty.current = false;
      setEditing(target);
    }
    if (target) setScrollToken(token => token + 1);
  };

  const toggle = async (provider: Row, enable: boolean) => {
    const id = String(provider.id);
    if (writing.current || loading || failed || switching) return;
    const name = String(provider.name || id);
    const routed = models.filter(model => model.target_provider_id === id).map(model => String(model.exposed_model_id ?? model.id));
    const confirmed = await confirmAction(enable ? {
      title: `启用 ${name}？`,
      consequence: routed.length ? `${routed.length} 个模型会重新使用这个供应商。` : '没有模型使用这个供应商。',
      confirmLabel: '启用',
    } : {
      title: `停用 ${name}？`,
      facts: routed.length ? [`路由到这里的模型：${routed.slice(0, 6).join('、')}${routed.length > 6 ? ` 等 ${routed.length} 个` : ''}`] : undefined,
      consequence: routed.length ? `这 ${routed.length} 个模型将没有可用线路，客户请求会失败。` : '没有模型使用这个供应商。',
      confirmLabel: '停用',
      danger: routed.length > 0,
    });
    if (!confirmed || writing.current) return;
    writing.current = true; setSwitching(id); reportError('');
    try {
      const response = await adminApi.updateProviderStatus(id, enable);
      if (!response.success) throw new Error('服务端未确认状态变更');
      toast.success(`已${enable ? '启用' : '停用'} ${name}`);
      await refresh();
    } catch (error) {
      reportError(`切换结果未确认：${errorText(error)}。请刷新核对供应商状态后再操作。`);
    } finally {writing.current = false; setSwitching(null);}
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
      return <section key={id} className="panel provider-card">
        <div className="provider-head">
          <div className="provider-name">
            <h3>{String(provider.name || id)}</h3>
            <span className="mono muted">{String(provider.base_url || '地址未配置')}</span>
            {typeof provider.api_type === 'string' && <Tag>{provider.api_type === 'openai' ? 'OpenAI' : provider.api_type === 'anthropic' ? 'Anthropic' : provider.api_type}</Tag>}
          </div>
          <Switch checked={enabled} label={`启用 ${String(provider.name || id)}`} busy={switching === id}
            disabled={!!switching || loading || failed} title={failed ? '供应商列表没有刷新成功，暂不能操作' : undefined}
            onChange={next => void toggle(provider, next)}/>
        </div>
        {keys.length ? <div className="table-scroll"><table className="table keys-table">
          <thead><tr><th className="col-key">Key</th><th>模型</th><th className="num col-weight">权重</th><th className="col-status">状态</th><th className="col-actions"><span className="sr-only">操作</span></th></tr></thead>
          <tbody>{keys.map(key => {
            const active = editing?.providerId === id && editing?.keyId === key.id;
            return <tr key={String(key.id)} className={active ? 'is-selected' : undefined}>
              <td className="mono nowrap col-key" title={String(key.id)}>{String(key.id)}</td>
              <td><ModelTags models={key.allowed_models}/></td>
              <td className="num">{String(key.weight ?? 1)}</td>
              <td className="col-status"><StatusBadge view={keyStatusView(key, nowSecs)}/></td>
              <td className="col-actions"><button type="button" className="btn-text" onClick={() => void edit({providerId: id, keyId: String(key.id)})}>编辑</button></td>
            </tr>;
          })}</tbody>
        </table></div> : <p className="muted">还没有 Key</p>}
        <div className="provider-foot"><button type="button" className="btn-text" onClick={() => void edit({providerId: id, suggestedKeyId: nextKeyId(id)})}>＋ 添加 Key</button></div>
      </section>;
    })}
    {!providers.length && <section className="panel"><ListState loading={loading} failed={failed} empty="还没有供应商" onRetry={() => void refresh()}
      action={<button type="button" className="btn btn-small" onClick={() => void edit({})}>添加供应商</button>}/></section>}

    {editing && <ProviderKeyEditor key={`${editing.providerId ?? 'new'}:${editing.keyId ?? 'new'}`} selectedKey={selectedKey} preset={editing}
      knownModels={models.map(model => String(model.target_model ?? '')).filter(Boolean)} knownProviders={providers.map(provider => String(provider.id))}
      onDirtyChange={onDirtyChange} onBusyChange={onBusyChange} onClose={() => void edit(null)}
      onSaved={saved => {
        if (saved) {mergeKey(saved); setEditing({providerId: String(saved.provider_id), keyId: String(saved.id)});}
        // A new provider was saved: its card (with its first Key) appears after the refresh.
        else {providerDirty.current = false; setEditing(null);}
        void refresh();
      }}/>}
  </div>;
}

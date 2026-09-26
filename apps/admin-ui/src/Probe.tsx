// 测试: one tiny real request to an upstream model through a provider (it costs a fraction of a
// cent), to see that the route answers before customers rely on it. Nothing is saved.
import {useEffect, useRef, useState} from 'react';
import {adminApi} from './api';
import {explainRefusal} from './refusal';
import {probeView, type StatusView} from './status';

export default function Probe({providerId, model, keyId, disabled, title}: {
  providerId: string;
  model: string;
  /** Test with this Key; without one, the server picks an enabled Key that allows the model. */
  keyId?: string;
  disabled?: boolean;
  title?: string;
}) {
  const [result, setResult] = useState<StatusView | 'running' | null>(null);
  const run = useRef(0);
  // A result belongs to the route it was run on.
  useEffect(() => {run.current++; setResult(null);}, [providerId, model, keyId]);
  const start = async () => {
    const id = ++run.current;
    setResult('running');
    try {
      const reply = await adminApi.probeKey({provider_id: providerId, model, ...(keyId ? {key_id: keyId} : {})});
      if (reply.success !== true) throw new Error('服务器未确认测试结果');
      const view = probeView(reply);
      // Which Key answered, when the server chose it.
      if (!keyId && typeof reply.key_id === 'string' && reply.key_id) view.label += ` · Key ${reply.key_id}`;
      if (id === run.current) setResult(view);
    } catch (error) {
      if (id === run.current) setResult({label: `测试没有完成：${explainRefusal(error instanceof Error ? error.message : String(error))}`, tone: 'danger'});
    }
  };
  return <span className="probe">
    <button type="button" className="btn-text btn-small" disabled={disabled || !providerId || !model || result === 'running'}
      title={title ?? '发一次很小的真实请求（花费不到 1 分钱），不保存任何东西'} onClick={() => void start()}>{result === 'running' ? '测试中…' : '测试'}</button>
    {result && result !== 'running' && <span role="status" className={`probe-result is-${result.tone}`} title={result.title}>{result.label}</span>}
  </span>;
}

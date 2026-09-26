// The one confirmation dialog: a title that names the action and its object, a few key
// facts, one line of consequence, and a button labelled with the verb. Focus starts on
// 取消. Irreversible bulk actions also ask the operator to type a word or a count.
import {useEffect, useRef, useState, type ReactNode} from 'react';
import {Modal} from './modal';

export interface ConfirmOptions {
  title: string;
  facts?: ReactNode[];
  body?: ReactNode;
  consequence?: ReactNode;
  confirmLabel: string;
  danger?: boolean;
  /** The operator types this exactly before the button works. */
  typed?: string;
  /** A short reason; its text is returned with the answer. Required ones must be filled in first. */
  reason?: {label: string; placeholder?: string; suggestions?: string[]; maxLength?: number; required?: boolean};
}

export interface ConfirmAnswer {confirmed: boolean; reason: string}

interface Request {options: ConfirmOptions; resolve: (answer: ConfirmAnswer) => void}

const NO: ConfirmAnswer = {confirmed: false, reason: ''};
let open: ((options: ConfirmOptions) => Promise<ConfirmAnswer>) | null = null;

/** Asks the operator; resolves to false when cancelled or when the workspace goes away. */
export function ask(options: ConfirmOptions): Promise<ConfirmAnswer> {
  return open ? open(options) : Promise.resolve(NO);
}

export async function confirmAction(options: ConfirmOptions): Promise<boolean> {
  return (await ask(options)).confirmed;
}

export function ConfirmHost() {
  const [request, setRequest] = useState<Request | null>(null);
  const current = useRef<Request | null>(null);
  useEffect(() => {
    open = options => new Promise<ConfirmAnswer>(resolve => {
      // One question at a time; a second one while the first is open is declined.
      if (current.current) {resolve(NO); return;}
      current.current = {options, resolve};
      setRequest(current.current);
    });
    return () => {
      open = null;
      current.current?.resolve(NO);
      current.current = null;
    };
  }, []);
  if (!request) return null;
  const finish = (answer: ConfirmAnswer) => {
    if (current.current !== request) return;
    current.current = null;
    setRequest(null);
    request.resolve(answer);
  };
  return <ConfirmDialog options={request.options} onFinish={finish}/>;
}

function ConfirmDialog({options, onFinish}: {options: ConfirmOptions; onFinish: (answer: ConfirmAnswer) => void}) {
  const [typed, setTyped] = useState('');
  const [reason, setReason] = useState('');
  const typedReady = !options.typed || typed.trim() === options.typed;
  const reasonReady = !options.reason?.required || !!reason.trim();
  const ready = typedReady && reasonReady;
  const accept = () => {if (ready) onFinish({confirmed: true, reason: reason.trim()});};
  return <Modal role="alertdialog" label={options.title} onClose={() => onFinish(NO)} className="confirm-dialog">
    <h3 className="modal-title">{options.title}</h3>
    {options.facts?.length ? <ul className="confirm-facts">{options.facts.map((fact, index) => <li key={index}>{fact}</li>)}</ul> : null}
    {options.body}
    {options.consequence && <p className={`confirm-consequence${options.danger ? ' is-danger' : ''}`}>{options.consequence}</p>}
    {options.reason && <div className="field">
      <label className="field-label" htmlFor="confirm-reason">{options.reason.label}{options.reason.required && <span className="required-mark">（必填）</span>}</label>
      <input id="confirm-reason" value={reason} maxLength={options.reason.maxLength ?? 200} placeholder={options.reason.placeholder}
        onChange={event => setReason(event.target.value)} onKeyDown={event => {if (event.key === 'Enter') accept();}}/>
      {!!options.reason.suggestions?.length && <div className="chips">
        {options.reason.suggestions.map(text => <button key={text} type="button" className="chip" aria-pressed={reason === text} onClick={() => setReason(text)}>{text}</button>)}
      </div>}
    </div>}
    {options.typed && <div className="field">
      <label className="field-label" htmlFor="confirm-typed">输入 <b className="mono">{options.typed}</b> 确认</label>
      <input id="confirm-typed" aria-label="确认输入" value={typed} autoComplete="off" spellCheck={false}
        onChange={event => setTyped(event.target.value)} onKeyDown={event => {if (event.key === 'Enter') accept();}}/>
    </div>}
    <div className="modal-actions">
      <button type="button" className="btn" data-autofocus onClick={() => onFinish(NO)}>取消</button>
      <button type="button" className={options.danger ? 'btn btn-danger-solid' : 'btn btn-primary'} data-confirm="accept"
        disabled={!ready} title={ready ? undefined : !reasonReady ? `填写原因后可以${options.confirmLabel}` : `输入 ${options.typed} 后可以${options.confirmLabel}`} onClick={accept}>{options.confirmLabel}</button>
    </div>
  </Modal>;
}

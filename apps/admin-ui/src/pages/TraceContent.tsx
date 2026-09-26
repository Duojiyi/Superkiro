// A request as it arrived (Kiro's conversation JSON) and the model's reply, read one
// request at a time from the 24-hour archive. Nothing here is stored or put in messages.
import {useState} from 'react';
import type {TraceContent, TraceReply} from '../api';
import {CopyButton, StatusBadge, Tag} from '../components/ui';
import {formatCount, formatDuration, formatSpeed, shortId} from '../format';
import {stopReasonView} from '../status';

type Json = Record<string, unknown>;
const isObject = (value: unknown): value is Json => !!value && typeof value === 'object' && !Array.isArray(value);
const list = (value: unknown): unknown[] => (Array.isArray(value) ? value : []);
const text = (value: unknown) => (typeof value === 'string' ? value : '');

export function prettyJson(value: unknown): string {
  if (typeof value === 'string') {
    try {return JSON.stringify(JSON.parse(value), null, 2);} catch {return value;}
  }
  try {return JSON.stringify(value, null, 2) ?? String(value);} catch {return String(value);}
}

const LINES_SHOWN = 20;

/** Text as written, long messages folded after 20 lines. */
function LongText({value}: {value: string}) {
  const [open, setOpen] = useState(false);
  const lines = value.split('\n');
  const long = lines.length > LINES_SHOWN;
  return <div className="long-text">
    <pre className="text-block">{long && !open ? lines.slice(0, LINES_SHOWN).join('\n') : value}</pre>
    {long && <button type="button" className="btn-text" onClick={() => setOpen(!open)}>{open ? '收起' : `展开（还有 ${lines.length - LINES_SHOWN} 行）`}</button>}
  </div>;
}

function argumentSummary(input: unknown): string {
  const value = typeof input === 'string' ? (() => {try {return JSON.parse(input);} catch {return input;}})() : input;
  if (isObject(value)) {
    const [key, first] = Object.entries(value)[0] ?? [];
    if (key === undefined) return '';
    const shown = typeof first === 'string' ? first : JSON.stringify(first);
    return `${key}: ${shown && shown.length > 40 ? `${shown.slice(0, 40)}…` : shown}`;
  }
  const shown = String(value ?? '');
  return shown.length > 40 ? `${shown.slice(0, 40)}…` : shown;
}

function ToolUse({use}: {use: Json}) {
  const summary = argumentSummary(use.input);
  return <details className="tool-block">
    <summary>调用 <b>{text(use.name) || '工具'}</b>{summary && <span className="muted">（{summary}）</span>}</summary>
    <pre className="code-block">{prettyJson(use.input)}</pre>
  </details>;
}

function ToolResult({result}: {result: Json}) {
  const failed = result.status === 'error';
  return <details className={`tool-block${failed ? ' is-failed' : ''}`}>
    <summary>工具结果 · {failed ? '失败' : '成功'}{text(result.toolUseId) && <span className="mono muted"> {shortId(text(result.toolUseId))}</span>}</summary>
    {list(result.content).map((item, index) => isObject(item) && typeof item.text === 'string'
      ? <LongText key={index} value={item.text}/>
      : <pre key={index} className="code-block">{prettyJson(isObject(item) && 'json' in item ? item.json : item)}</pre>)}
  </details>;
}

function Images({images}: {images: unknown[]}) {
  if (!images.length) return null;
  return <p className="image-tags">{images.map((image, index) => {
    const bytes = isObject(image) && isObject(image.source) ? text(image.source.bytes) : '';
    const format = isObject(image) ? text(image.format) : '';
    return <Tag key={index}>{bytes.startsWith('[') ? bytes.replace(/^\[|\]$/g, '') : `图片${format ? ` · ${format}` : ''}`}</Tag>;
  })}</p>;
}

function Turn({entry, current}: {entry: Json; current?: boolean}) {
  if (isObject(entry.userInputMessage)) {
    const message = entry.userInputMessage;
    const context = isObject(message.userInputMessageContext) ? message.userInputMessageContext : {};
    const results = list(context.toolResults).filter(isObject);
    return <article className={`turn turn-user${current ? ' is-current' : ''}`}>
      <header className="turn-head">用户{current && <Tag tone="info">本次请求</Tag>}</header>
      {text(message.content) && <LongText value={text(message.content)}/>}
      <Images images={list(message.images)}/>
      {results.map((result, index) => <ToolResult key={index} result={result}/>)}
      {!text(message.content) && !results.length && !list(message.images).length && <p className="muted">（空）</p>}
    </article>;
  }
  if (isObject(entry.assistantResponseMessage)) {
    const message = entry.assistantResponseMessage;
    const uses = list(message.toolUses).filter(isObject);
    return <article className="turn turn-model">
      <header className="turn-head">模型</header>
      {text(message.content) && <LongText value={text(message.content)}/>}
      {uses.map((use, index) => <ToolUse key={index} use={use}/>)}
      {!text(message.content) && !uses.length && <p className="muted">（空）</p>}
    </article>;
  }
  return <article className="turn"><pre className="code-block">{prettyJson(entry)}</pre></article>;
}

function toolName(tool: unknown): {name: string; description: string} {
  const spec = isObject(tool) ? (isObject(tool.toolSpecification) ? tool.toolSpecification : tool) : {};
  return {name: text(spec.name) || '未命名', description: text(spec.description)};
}

/** The request, parsed; `null` when it is not a Kiro conversation we can read. */
function parseRequest(content: TraceContent) {
  const notes = content.notes ?? {};
  const request = isObject(content.request) ? content.request : {};
  if (notes.unparsed || typeof request.unparsed === 'string' || notes.truncated || request.tooLarge) return null;
  const state = isObject(request.conversationState) ? request.conversationState : null;
  if (!state) return null;
  const history = list(state.history).filter(isObject);
  const currentMessage = isObject(state.currentMessage) && isObject(state.currentMessage.userInputMessage) ? state.currentMessage.userInputMessage : null;
  const omitted = [
    notes.omittedHistoryEntries ? `${formatCount(notes.omittedHistoryEntries)} 条较早的历史` : '',
    notes.omittedImages ? `${formatCount(notes.omittedImages)} 张图片的内容` : '',
  ].filter(Boolean);
  return {history, currentMessage, omitted};
}

/** This turn first: what the customer asked, then what the model answered; earlier turns folded. */
export function ConversationView({content}: {content: TraceContent}) {
  const parsed = parseRequest(content);
  if (!parsed) return <RequestView content={content}/>;
  const {history, currentMessage, omitted} = parsed;
  const rounds = history.filter(entry => isObject(entry.userInputMessage)).length;
  return <div className="conversation">
    {currentMessage ? <Turn entry={{userInputMessage: currentMessage}} current/> : <p className="muted">没有本次输入</p>}
    <article className="turn turn-model is-reply">
      <header className="turn-head">模型回复</header>
      <ReplyView reply={content.reply ?? null} compact/>
    </article>
    {omitted.length > 0 && <p className="muted">已省略 {omitted.join('、')}</p>}
    {rounds > 0 && <details className="history-block">
      <summary>展开 {formatCount(rounds)} 轮历史</summary>
      <RequestView content={content} hideCurrent/>
    </details>}
  </div>;
}

export function RequestView({content, hideCurrent}: {content: TraceContent; hideCurrent?: boolean}) {
  const notes = content.notes ?? {};
  const request = isObject(content.request) ? content.request : {};
  const omitted = [
    notes.omittedHistoryEntries ? `${formatCount(notes.omittedHistoryEntries)} 条较早的历史` : '',
    notes.omittedImages ? `${formatCount(notes.omittedImages)} 张图片的内容` : '',
  ].filter(Boolean);
  if (notes.unparsed || typeof request.unparsed === 'string') {
    return <div className="conversation">
      <p className="note-warning">请求无法解析，只保留了前 64 KB 原文</p>
      <pre className="code-block">{text(request.unparsed)}</pre>
    </div>;
  }
  if (notes.truncated || request.tooLarge) return <div className="conversation"><p className="note-warning">请求过大，没有保存内容</p></div>;
  const state = isObject(request.conversationState) ? request.conversationState : null;
  if (!state) return <div className="conversation"><pre className="code-block">{prettyJson(content.request)}</pre></div>;
  const history = list(state.history).filter(isObject);
  const currentMessage = isObject(state.currentMessage) && isObject(state.currentMessage.userInputMessage) ? state.currentMessage.userInputMessage : null;
  const contexts = [currentMessage, ...history.map(entry => entry.userInputMessage)].filter(isObject).map(message => message.userInputMessageContext).filter(isObject);
  const tools = contexts.map(context => list(context.tools)).find(entries => entries.length) ?? [];
  return <div className="conversation">
    {omitted.length > 0 && !hideCurrent && <p className="muted">已省略 {omitted.join('、')}</p>}
    {text(request.systemPrompt) && <details className="tool-block"><summary>系统提示词</summary><LongText value={text(request.systemPrompt)}/></details>}
    {tools.length > 0 && <details className="tool-block">
      <summary>{tools.length} 个可用工具</summary>
      <ul className="tool-list">{tools.map((tool, index) => {
        const {name, description} = toolName(tool);
        return <li key={index}><b className="mono">{name}</b>{description && <span className="muted"> {description.length > 120 ? `${description.slice(0, 120)}…` : description}</span>}</li>;
      })}</ul>
      <details className="tool-block"><summary>工具定义 JSON</summary><pre className="code-block">{prettyJson(tools)}</pre></details>
    </details>}
    {history.map((entry, index) => <Turn key={index} entry={entry}/>)}
    {currentMessage && !hideCurrent && <Turn entry={{userInputMessage: currentMessage}} current/>}
  </div>;
}

export function ReplyView({reply, compact}: {reply: TraceReply | null; compact?: boolean}) {
  if (!reply) return <p className="empty-note">没有记录到回复（请求未完成，或在开始输出前失败）</p>;
  const stop = stopReasonView(reply.stopReason);
  const calls = list(reply.toolCalls).filter(isObject) as TraceReply['toolCalls'];
  return <div className="reply">
    {!compact && <p className="reply-meta">{stop && <StatusBadge view={stop}/>}<span className="mono muted">{[reply.providerId, reply.targetModel].filter(Boolean).join(' / ')}</span></p>}
    {reply.error && <p className="form-error">{reply.error}</p>}
    {reply.truncated && <p className="note-warning">回复超长，只保留了前 1 MB</p>}
    {reply.text ? <section className="text-section">
      <div className="block-head"><span>回复</span><CopyButton text={reply.text}/></div>
      <pre className="text-block">{reply.text}</pre>
    </section> : <p className="muted">没有文字回复</p>}
    {reply.reasoning && <details className="tool-block reasoning">
      <summary>思考过程（{formatCount([...reply.reasoning].length)} 字）</summary>
      <pre className="text-block">{reply.reasoning}</pre>
    </details>}
    {calls.length > 0 && <section className="text-section">
      <div className="block-head"><span>工具调用 {calls.length}</span></div>
      {calls.map((call, index) => <details key={call.id || index} className="tool-block" open={calls.length <= 3}>
        <summary><b>{call.name || '工具'}</b>{call.id && <span className="mono muted"> {shortId(call.id)}</span>}</summary>
        <pre className="code-block">{prettyJson(call.arguments)}</pre>
        <CopyButton text={prettyJson(call.arguments)} label="复制参数"/>
      </details>)}
    </section>}
    {!compact && <dl className="usage-grid">
      <div><dt>输入</dt><dd>{formatCount(reply.inputTokens)}</dd></div>
      <div><dt>输出</dt><dd>{formatCount(reply.outputTokens)}</dd></div>
      <div><dt>缓存读</dt><dd>{formatCount(reply.cacheReadTokens)}</dd></div>
      <div><dt>缓存写</dt><dd>{formatCount(reply.cacheWriteTokens)}</dd></div>
      <div><dt>首字</dt><dd>{formatDuration(reply.ttftMs)}</dd></div>
      <div><dt>速度</dt><dd>{formatSpeed(reply.tokensPerSecond)}</dd></div>
    </dl>}
    {compact && stop && <p className="reply-meta"><StatusBadge view={stop}/></p>}
  </div>;
}

const RAW_LIMIT = 500_000;

export function RawView({content}: {content: TraceContent}) {
  const json = prettyJson(content);
  return <div className="raw-json">
    <div className="block-head"><span>{json.length > RAW_LIMIT ? '内容较长，这里只显示前 500 KB；复制可得到全部' : '接口返回的完整内容'}</span><CopyButton text={json} label="复制 JSON"/></div>
    <pre className="code-block">{json.length > RAW_LIMIT ? json.slice(0, RAW_LIMIT) : json}</pre>
  </div>;
}

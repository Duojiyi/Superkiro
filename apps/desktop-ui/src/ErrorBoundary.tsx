import { listen } from '@tauri-apps/api/event';
import { Component, useEffect, useState, type ReactNode } from 'react';
import { native } from './bridge';
import { toClientError } from './errors';
import { UpdateScreen, useUpdater } from './Updater';

const DOWNLOADS = 'https://kiro.rent/#downloads';

/** In place of a page that failed to render: a way back, a way out, window controls, and
 * updates still working, so a release whose page breaks is replaced by the one that fixes it. */
function CrashPage({ reload }: { reload: () => void }) {
  const updater = useUpdater(false, () => {});
  const [problem, setProblem] = useState('');
  const call = (method: string, args: unknown[] = []) => void native(method, args).catch(e => setProblem(toClientError(e).message));
  // The tray's exit comes here too: with the page gone, nothing else would answer it.
  useEffect(() => {
    let disposed = false, stop: (() => void) | undefined;
    void listen('desktop-exit-request', () => { void native('exit').catch(e => setProblem(toClientError(e).message)); })
      .then(unlisten => { if (disposed) unlisten(); else stop = unlisten; }).catch(() => {});
    return () => { disposed = true; stop?.(); };
  }, []);
  const openDownloads = () => call('open_external', [DOWNLOADS]);
  return <div className="shell">
    <header onMouseDown={event => { if (event.button === 0 && !(event.target as HTMLElement).closest('button')) void native('drag').catch(() => {}); }}>
      <strong className="brand">Superkiro</strong><div className="window-drag"/>
      <div className="window-controls">
        <button aria-label="最小化" onClick={() => call('minimize')}>−</button>
        <button aria-label="关闭窗口" onClick={() => call('close')}>×</button>
      </div>
    </header>
    {updater.screen
      ? <main className="page-update"><UpdateScreen updater={updater} blocked={false} openDownloads={openDownloads} restore={null}/></main>
      : <main className="page-crash"><section>
        <h1>界面出现错误</h1>
        <p className="subtitle">Kiro 的连接配置不受影响。重新加载界面即可继续；仍然出错时，请更新到新版本。</p>
        {problem && <p className="warning" role="alert">{problem}</p>}
        <button className="primary full" onClick={reload}>重新加载界面</button>
        {updater.offer && <button className="full" onClick={() => updater.start()}>更新到 {updater.offer.version}</button>}
        <button className="full" onClick={openDownloads}>从官网下载新版 ↗</button>
        <button className="text full" onClick={() => call('exit')}>退出 Superkiro</button>
      </section></main>}
  </div>;
}

export class ErrorBoundary extends Component<{ children: ReactNode; reload?: () => void }, { failed: boolean }> {
  state = { failed: false };
  static getDerivedStateFromError() { return { failed: true }; }
  render() {
    if (!this.state.failed) return this.props.children;
    return <CrashPage reload={this.props.reload ?? (() => window.location.reload())}/>;
  }
}

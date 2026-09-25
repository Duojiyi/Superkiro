// @vitest-environment jsdom
import { act, cleanup, fireEvent, render, screen } from '@testing-library/react';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { listen } from '@tauri-apps/api/event';
import { native } from './bridge';
import { ErrorBoundary } from './ErrorBoundary';

vi.mock('./bridge', () => ({ native: vi.fn() }));
vi.mock('@tauri-apps/api/event', () => ({ listen: vi.fn(async () => () => {}) }));
const call = vi.mocked(native);
const flush = async () => { await act(async () => { for (let i = 0; i < 5; i++) await Promise.resolve(); }); };
const current = { state: 'current', current: '2026.09.25' };

function host(check: unknown, refused: Record<string, unknown> = {}) {
  call.mockImplementation(async (method: string) => {
    if (method in refused) throw refused[method];
    if (method === 'update_check') return check;
    if (method === 'update_install') return new Promise(() => {});
    return null;
  });
}

function Broken(): never { throw new Error('render failed'); }

beforeEach(() => { call.mockReset(); vi.mocked(listen).mockClear(); vi.spyOn(console, 'error').mockImplementation(() => {}); });
afterEach(() => { cleanup(); vi.restoreAllMocks(); });

describe('ErrorBoundary', () => {
  it('shows the page while it renders', () => {
    host(current);
    render(<ErrorBoundary><p>页面内容</p></ErrorBoundary>);
    expect(screen.getByText('页面内容')).toBeTruthy();
  });

  it('a page that fails to render offers a reload, the downloads, a way out and window controls', async () => {
    host(current);
    const reload = vi.fn();
    render(<ErrorBoundary reload={reload}><Broken/></ErrorBoundary>); await flush();
    expect(screen.getByRole('heading', { name: '界面出现错误' })).toBeTruthy();
    fireEvent.click(screen.getByRole('button', { name: '重新加载界面' }));
    expect(reload).toHaveBeenCalledOnce();
    fireEvent.click(screen.getByRole('button', { name: '从官网下载新版 ↗' }));
    expect(call).toHaveBeenCalledWith('open_external', ['https://kiro.rent/#downloads']);
    fireEvent.click(screen.getByRole('button', { name: '最小化' }));
    fireEvent.click(screen.getByRole('button', { name: '关闭窗口' }));
    fireEvent.click(screen.getByRole('button', { name: '退出 Superkiro' }));
    for (const method of ['minimize', 'close', 'exit']) expect(call).toHaveBeenCalledWith(method, []);
    // A page that never rendered its status confirms nothing.
    expect(call).not.toHaveBeenCalledWith('update_confirm');
  });

  it('an exit the host refuses says why', async () => {
    host(current, { exit: { code: 'SK-LOCAL-002', feedback_id: 'sk-1-2-3', stage: 'native', outcome: 'failed', occurred_at: '1790000000' } });
    render(<ErrorBoundary><Broken/></ErrorBoundary>); await flush();
    fireEvent.click(screen.getByRole('button', { name: '退出 Superkiro' })); await flush();
    expect(screen.getByRole('alert').textContent).toBeTruthy();
    // The tray's exit is answered here as well.
    const handler = vi.mocked(listen).mock.calls.find(([event]) => event === 'desktop-exit-request')![1];
    await act(async () => { handler({ event: 'desktop-exit-request', id: 1, payload: null }); }); await flush();
    expect(call.mock.calls.filter(([m]) => m === 'exit')).toHaveLength(2);
  });

  it('a broken page still updates itself to the release that fixes it', async () => {
    host({ state: 'available', current: '2026.09.25', version: '2026.09.26', size: 4 * 1024 * 1024, mandatory: true });
    render(<ErrorBoundary><Broken/></ErrorBoundary>); await flush();
    expect(screen.getByRole('heading', { name: '正在更新 Superkiro' })).toBeTruthy();
    expect(call).toHaveBeenCalledWith('update_install');
  });

  it('an optional release is offered from the crash page', async () => {
    host({ state: 'available', current: '2026.09.25', version: '2026.09.26', size: 4 * 1024 * 1024, mandatory: false });
    render(<ErrorBoundary><Broken/></ErrorBoundary>); await flush();
    fireEvent.click(screen.getByRole('button', { name: '更新到 2026.09.26' })); await flush();
    expect(call).toHaveBeenCalledWith('update_install');
  });
});

// @vitest-environment jsdom
import { act, cleanup, fireEvent, render, screen } from '@testing-library/react';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { listen } from '@tauri-apps/api/event';
import { native } from './bridge';
import { parseCheck, UpdateScreen, useUpdater } from './Updater';

vi.mock('./bridge', () => ({ native: vi.fn() }));
vi.mock('@tauri-apps/api/event', () => ({ listen: vi.fn(async () => () => {}) }));
const call = vi.mocked(native);
const flush = async () => { await act(async () => { for (let i = 0; i < 5; i++) await Promise.resolve(); }); };
const mandatory = { state: 'available', current: '2026.09.22', version: '2026.09.25', size: 4 * 1024 * 1024, mandatory: true };

function Harness({ blocked = false, onUpdated = () => {}, restore = null }: { blocked?: boolean; onUpdated?: (v: string) => void; restore?: (() => void) | null }) {
  const updater = useUpdater(blocked, onUpdated);
  return <div>
    {updater.offer && <button onClick={() => updater.start()}>{updater.postponed ? '继续更新' : `新版本 ${updater.offer.version}`} ↑</button>}
    {updater.screen ? <UpdateScreen updater={updater} blocked={blocked} openDownloads={() => call('open_external')} restore={restore}/> : <p>页面内容</p>}
  </div>;
}

function host(check: unknown, install: () => Promise<unknown> = () => new Promise(() => {})) {
  call.mockImplementation(async (method: string) => {
    if (method === 'update_check') return check;
    if (method === 'update_install') return install();
    return true;
  });
}

const failure = (code: string) => ({ code, feedback_id: 'sk-1-2-3', stage: 'native', outcome: 'failed', occurred_at: '1790000000' });

beforeEach(() => { call.mockReset(); vi.mocked(listen).mockClear(); });
afterEach(() => { cleanup(); vi.useRealTimers(); });

describe('parseCheck', () => {
  it('accepts only what the host sends', () => {
    expect(parseCheck({ state: 'disabled' })).toEqual({ state: 'disabled', updated: false });
    expect(parseCheck({ state: 'current', current: '2026.09.25', updated: true })).toEqual({ state: 'current', current: '2026.09.25', updated: true });
    expect(parseCheck(mandatory)).toEqual({ ...mandatory, updated: false });
    for (const bad of [true, null, 'available', { state: 'available', current: '1', version: 'latest', size: 1, mandatory: true },
      { ...mandatory, size: -1 }, { ...mandatory, size: 1.5 }, { ...mandatory, mandatory: 'yes' }, { state: 'current' }, { state: 'other' }]) {
      expect(parseCheck(bad)).toBeNull();
    }
  });
});

describe('useUpdater and UpdateScreen', () => {
  it('confirms this build once the host has answered, and only once', async () => {
    host({ state: 'current', current: '2026.09.25' });
    render(<Harness/>); await flush();
    expect(call.mock.calls.filter(([m]) => m === 'update_confirm')).toHaveLength(1);
    // Confirmation follows a host answer: a host that never answers is never confirmed.
    cleanup(); call.mockReset(); call.mockRejectedValue(new Error('host down'));
    render(<Harness/>); await flush();
    expect(call).not.toHaveBeenCalledWith('update_confirm');
  });

  it('a mandatory update takes the place of the page and installs by itself', async () => {
    host(mandatory);
    render(<Harness/>); await flush();
    expect(screen.queryByText('页面内容')).toBeNull();
    expect(screen.getByRole('heading', { name: '正在更新 Superkiro' })).toBeTruthy();
    expect(screen.getByText(/新版本 2026.09.25（当前 2026.09.22）为必需更新/)).toBeTruthy();
    expect(call).toHaveBeenCalledWith('update_install');
  });

  it('waits for an operation in progress before installing', async () => {
    host(mandatory);
    const view = render(<Harness blocked/>); await flush();
    expect(screen.getByRole('heading', { name: '需要更新 Superkiro' })).toBeTruthy();
    expect(screen.getByText(/正在等待当前操作完成/)).toBeTruthy();
    expect(call).not.toHaveBeenCalledWith('update_install');
    view.rerender(<Harness/>); await flush();
    expect(call).toHaveBeenCalledWith('update_install');
  });

  it('shows animated download progress from the host', async () => {
    host(mandatory);
    render(<Harness/>); await flush();
    const handler = vi.mocked(listen).mock.calls.find(([event]) => event === 'update-progress')![1];
    act(() => { handler({ event: 'update-progress', id: 1, payload: { received: 0, total: 4 * 1024 * 1024, resumed: false } }); });
    act(() => { handler({ event: 'update-progress', id: 2, payload: { received: 2 * 1024 * 1024, total: 4 * 1024 * 1024, resumed: false } }); });
    expect(screen.getByText('50%')).toBeTruthy();
    expect(screen.getByText(/已下载 2 \/ 4 MB/)).toBeTruthy();
    expect(screen.getByRole('progressbar').getAttribute('aria-valuenow')).toBe('50');
    // Started from zero, so it is not a resumed download.
    expect(screen.queryByText(/已从断点续传/)).toBeNull();
    // Nonsense from the event channel is ignored.
    act(() => { handler({ event: 'update-progress', id: 3, payload: { received: 9, total: 1 } }); });
    expect(screen.getByText('50%')).toBeTruthy();
  });

  it('marks a download the host says resumed from a break point', async () => {
    host(mandatory);
    render(<Harness/>); await flush();
    const handler = vi.mocked(listen).mock.calls.find(([event]) => event === 'update-progress')![1];
    act(() => { handler({ event: 'update-progress', id: 1, payload: { received: 1 * 1024 * 1024, total: 4 * 1024 * 1024, resumed: true } }); });
    expect(screen.getByText(/已从断点续传/)).toBeTruthy();
    // A fresh download that happens to report progress first is not called resumed.
    act(() => { handler({ event: 'update-progress', id: 2, payload: { received: 2 * 1024 * 1024, total: 4 * 1024 * 1024, resumed: false } }); });
    expect(screen.queryByText(/已从断点续传/)).toBeNull();
  });

  it('a download in progress can be put off, which stops it for later', async () => {
    host(mandatory);
    render(<Harness/>); await flush();
    expect(screen.getByRole('heading', { name: '正在更新 Superkiro' })).toBeTruthy();
    fireEvent.click(screen.getByRole('button', { name: '暂停更新，先进入客户端' })); await flush();
    expect(call).toHaveBeenCalledWith('update_cancel');
    expect(screen.getByText('页面内容')).toBeTruthy();
    expect(screen.getByRole('button', { name: '继续更新 ↑' })).toBeTruthy();
  });

  it('a retry waits from now, however long the client has been open', async () => {
    vi.useFakeTimers();
    let attempts = 0;
    host({ state: 'current', current: '2026.09.22' });
    const view = render(<Harness/>); await flush();
    // Two days in the tray, then a required update whose install finds a takeover running.
    await act(async () => { vi.advanceTimersByTime(2 * 24 * 3600 * 1000); }); await flush();
    host(mandatory, async () => { attempts++; throw failure('SK-LOCAL-002'); });
    view.rerender(<Harness key="later"/>); await flush();
    expect(attempts).toBe(1);
    await act(async () => { vi.advanceTimersByTime(31_000); }); await flush();
    expect(attempts).toBe(2);
  });

  it('once the new version is in place, says the client is restarting', async () => {
    host(mandatory, async () => true);
    render(<Harness/>); await flush();
    expect(screen.getByRole('heading', { name: '正在重启 Superkiro' })).toBeTruthy();
  });

  it('a dropped line resumes automatically, then gives up after a few tries', async () => {
    vi.useFakeTimers();
    let attempts = 0;
    host(mandatory, async () => { attempts++; if (attempts <= 2) throw failure('SK-UPDATE-001'); return new Promise(() => {}); });
    render(<Harness/>); await flush();
    expect(attempts).toBe(1);
    expect(screen.getByText(/网络中断，正在自动断点续传重试/)).toBeTruthy();
    await act(async () => { vi.advanceTimersByTime(3_000); }); await flush();
    expect(attempts).toBe(2);
    await act(async () => { vi.advanceTimersByTime(3_000); }); await flush();
    // The third attempt gets through and proceeds to install (no failure screen).
    expect(attempts).toBe(3);
    expect(screen.getByRole('heading', { name: '正在更新 Superkiro' })).toBeTruthy();
    expect(screen.queryByRole('heading', { name: '更新未完成' })).toBeNull();
  });

  it('after too many dropped lines it stops and asks the customer', async () => {
    vi.useFakeTimers();
    let attempts = 0;
    host(mandatory, async () => { attempts++; throw failure('SK-UPDATE-001'); });
    render(<Harness/>); await flush();
    for (let i = 0; i < 4; i++) { await act(async () => { vi.advanceTimersByTime(3_000); }); await flush(); }
    // One initial try plus three automatic resumes, then the failure screen.
    expect(attempts).toBe(4);
    expect(screen.getByRole('heading', { name: '更新未完成' })).toBeTruthy();
  });

  it('refused by an operation in progress, tries again half a minute later', async () => {
    vi.useFakeTimers();
    let attempts = 0;
    host(mandatory, async () => { attempts++; throw failure('SK-LOCAL-002'); });
    render(<Harness/>); await flush();
    expect(attempts).toBe(1);
    expect(screen.queryByText('更新未完成')).toBeNull();
    await act(async () => { vi.advanceTimersByTime(29_000); }); await flush();
    expect(attempts).toBe(1);
    await act(async () => { vi.advanceTimersByTime(2_000); }); await flush();
    expect(attempts).toBe(2);
  });

  it('a failed update explains itself and offers retry, download and restore', async () => {
    let fail = true;
    host(mandatory, async () => { if (fail) throw failure('SK-UPDATE-003'); return new Promise(() => {}); });
    // SK-UPDATE-003 is a local failure: no automatic retries, the screen asks at once.
    const restore = vi.fn();
    render(<Harness restore={restore}/>); await flush();
    expect(screen.getByRole('heading', { name: '更新未完成' })).toBeTruthy();
    expect(screen.getByText(/无法替换当前的程序文件/)).toBeTruthy();
    expect(screen.getByText(/SK-UPDATE-003/)).toBeTruthy();
    fireEvent.click(screen.getByRole('button', { name: '还原 Kiro 配置' }));
    expect(restore).toHaveBeenCalledTimes(1);
    fireEvent.click(screen.getByRole('button', { name: '从官网下载新版 ↗' }));
    expect(call).toHaveBeenCalledWith('open_external');
    fail = false;
    fireEvent.click(screen.getByRole('button', { name: '重试更新' })); await flush();
    expect(screen.getByRole('heading', { name: '正在更新 Superkiro' })).toBeTruthy();
  });

  it('a failed mandatory update can be put off so the client is never locked out', async () => {
    host(mandatory, async () => { throw failure('SK-UPDATE-004'); });
    render(<Harness/>); await flush();
    expect(screen.getByRole('heading', { name: '更新未完成' })).toBeTruthy();
    // Even required, it yields to the client, then keeps offering from the header.
    fireEvent.click(screen.getByRole('button', { name: '暂时进入客户端' })); await flush();
    expect(screen.getByText('页面内容')).toBeTruthy();
    expect(screen.getByRole('button', { name: '继续更新 ↑' })).toBeTruthy();
    // It does not auto-reinstall on its own after being put off.
    const installs = call.mock.calls.filter(([m]) => m === 'update_install').length;
    await flush();
    expect(call.mock.calls.filter(([m]) => m === 'update_install').length).toBe(installs);
    // The header resumes it on demand.
    fireEvent.click(screen.getByRole('button', { name: '继续更新 ↑' })); await flush();
    expect(screen.getByRole('heading', { name: '更新未完成' })).toBeTruthy();
  });

  it('while waiting to start, restoring Kiro stays reachable', async () => {
    host(mandatory);
    const restore = vi.fn();
    render(<Harness blocked restore={restore}/>); await flush();
    fireEvent.click(screen.getByRole('button', { name: '还原 Kiro 配置' }));
    expect(restore).toHaveBeenCalledTimes(1);
  });

  it('an optional update waits for the customer and can be put off after a failure', async () => {
    host({ ...mandatory, mandatory: false }, async () => { throw failure('SK-UPDATE-001'); });
    render(<Harness/>); await flush();
    expect(screen.getByText('页面内容')).toBeTruthy();
    expect(call).not.toHaveBeenCalledWith('update_install');
    fireEvent.click(screen.getByRole('button', { name: '新版本 2026.09.25 ↑' })); await flush();
    expect(screen.getByRole('heading', { name: '更新未完成' })).toBeTruthy();
    fireEvent.click(screen.getByRole('button', { name: '暂时进入客户端' }));
    expect(screen.getByText('页面内容')).toBeTruthy();
  });

  it('nothing to install, a failed check or a build that does not update shows nothing', async () => {
    for (const check of [{ state: 'current', current: '2026.09.25' }, { state: 'disabled' }, true]) {
      host(check);
      render(<Harness/>); await flush();
      expect(screen.getByText('页面内容')).toBeTruthy();
      expect(screen.queryByRole('button', { name: /新版本|继续更新/ })).toBeNull();
      cleanup();
    }
    call.mockRejectedValue(new Error('offline'));
    render(<Harness/>); await flush();
    expect(screen.getByText('页面内容')).toBeTruthy();
  });

  it('checks again every half hour and reports a completed update once', async () => {
    vi.useFakeTimers();
    const onUpdated = vi.fn();
    host({ state: 'current', current: '2026.09.25', updated: true });
    render(<Harness onUpdated={onUpdated}/>); await flush();
    expect(onUpdated).toHaveBeenCalledWith('2026.09.25');
    await act(async () => { vi.advanceTimersByTime(30 * 60 * 1000); }); await flush();
    expect(call.mock.calls.filter(([method]) => method === 'update_check')).toHaveLength(2);
    expect(onUpdated).toHaveBeenCalledTimes(1);
  });
});

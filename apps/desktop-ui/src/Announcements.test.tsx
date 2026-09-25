// @vitest-environment jsdom
import { act, cleanup, fireEvent, render, screen } from '@testing-library/react';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { Announcements, paginateText } from './Announcements';
import { api } from './bridge';
vi.mock('./bridge', () => ({ api: vi.fn() }));
const request = vi.mocked(api);
const STORAGE = 'superkiro.announcements.read.v1';
const item = { id: 'one', title: '服务通知', content: '公开内容', level: 'info', created_at: 1, expires_at: null };
const flush = async () => { await act(async () => { await Promise.resolve(); }); };
const open = () => fireEvent.click(screen.getByRole('button', { name: /^公告/ }));
const modal = () => document.querySelector<HTMLDialogElement>('.announcements-dialog')!;
beforeEach(() => {
  const values = new Map<string, string>();
  // Do not read native/window.localStorage: Node's experimental implementation
  // can shadow jsdom's storage. Install a fresh in-memory Storage for every test.
  const mockStorage: Storage = {
    get length() { return values.size; },
    clear: () => values.clear(),
    getItem: key => values.get(String(key)) ?? null,
    key: index => Array.from(values.keys())[index] ?? null,
    removeItem: key => { values.delete(String(key)); },
    setItem: (key, value) => { values.set(String(key), String(value)); },
  };
  vi.stubGlobal('localStorage', mockStorage);
  request.mockReset().mockResolvedValue({ announcements: [item] });
  vi.spyOn(document, 'hasFocus').mockReturnValue(true);
  HTMLDialogElement.prototype.showModal = function () { this.open = true; };
  HTMLDialogElement.prototype.close = function () { this.open = false; };
  vi.stubGlobal('ResizeObserver', class { observe() {} disconnect() {} });
  vi.spyOn(HTMLElement.prototype, 'clientWidth', 'get').mockReturnValue(300);
  vi.spyOn(HTMLElement.prototype, 'clientHeight', 'get').mockReturnValue(100);
  vi.spyOn(HTMLElement.prototype, 'scrollWidth', 'get').mockReturnValue(300);
  vi.spyOn(HTMLElement.prototype, 'scrollHeight', 'get').mockImplementation(function (this: HTMLElement) {
    return Math.ceil(Array.from(this.textContent || '').length / 20) * 25;
  });
});
afterEach(() => { cleanup(); vi.restoreAllMocks(); vi.unstubAllGlobals(); vi.useRealTimers(); });

describe('Announcements', () => {
  it('uses the same isolated mock storage for global and window access', () => {
    expect(window.localStorage).toBe(localStorage);
    expect(localStorage.length).toBe(0);
    window.localStorage.setItem('example', 'value');
    expect(localStorage.getItem('example')).toBe('value');
    expect(localStorage.key(0)).toBe('example');
    localStorage.removeItem('example');
    expect(window.localStorage.getItem('example')).toBeNull();
  });
  it('splits Unicode and newlines losslessly', () => {
    const text = '标题😀\n\n' + '超长正文\n'.repeat(80);
    const pages = paginateText(text, s => Array.from(s).length <= 17);
    expect(pages.join('')).toBe(text);
    expect(pages.every(s => Array.from(s).length <= 17)).toBe(true);
    expect(paginateText('', () => true)).toEqual(['']);
  });
  it('fetches immediately, polls every five minutes, and cleans up', async () => {
    vi.useFakeTimers();
    const view = render(<Announcements blocked />); await flush();
    expect(request).toHaveBeenCalledWith('/api/announcements');
    await act(async () => { vi.advanceTimersByTime(300000); });
    expect(request).toHaveBeenCalledTimes(2);
    view.unmount();
    await act(async () => { vi.advanceTimersByTime(300000); });
    expect(request).toHaveBeenCalledTimes(2);
  });
  it('supports manual reopening and Escape while blocked', async () => {
    render(<Announcements blocked />); await flush();
    expect(modal().open).toBe(false); open(); expect(modal().open).toBe(true);
    fireEvent(modal(), new Event('cancel', { cancelable: true }));
    expect(modal().open).toBe(false); open(); expect(modal().open).toBe(true);
  });
  it('does not interrupt input focus or other dialogs', async () => {
    const input = document.createElement('input'); document.body.append(input); input.focus();
    const view = render(<Announcements blocked={false} />); await flush();
    expect(modal().open).toBe(false); expect(document.activeElement).toBe(input);
    view.unmount(); input.remove();
    const other = document.createElement('dialog'); other.open = true; document.body.append(other);
    render(<Announcements blocked={false} />); await flush(); expect(modal().open).toBe(false);
    other.remove(); await flush(); expect(modal().open).toBe(true);
  });
  it('yields an automatic dialog when a blocking workflow starts', async () => {
    const view = render(<Announcements blocked={false} />); await flush(); expect(modal().open).toBe(true);
    view.rerender(<Announcements blocked />); await flush(); expect(modal().open).toBe(false);
  });
  it('persists exact content versions and detects edits with the same id', async () => {
    const first = render(<Announcements blocked={false} />); await flush();
    expect(modal().open).toBe(true); expect(JSON.parse(localStorage.getItem(STORAGE)!)['one']).toBeTruthy();
    // The read state holds a hash, never the announcement's text.
    expect(localStorage.getItem(STORAGE)).not.toContain('公开内容'); first.unmount();
    const second = render(<Announcements blocked={false} />); await flush(); expect(modal().open).toBe(false); second.unmount();
    request.mockResolvedValue({ announcements: [{ ...item, content: '更新内容' }] });
    render(<Announcements blocked={false} />); await flush(); expect(modal().open).toBe(true);
  });
  it('forgets announcements that left the feed and honours read marks from earlier releases', async () => {
    localStorage.setItem(STORAGE, JSON.stringify({ gone: 'x', one: JSON.stringify([item.title, item.content, item.level]) }));
    const first = render(<Announcements blocked={false} />); await flush();
    // Read under the old text-valued record: not shown again.
    expect(modal().open).toBe(false); first.unmount();
    request.mockResolvedValue({ announcements: [item, { ...item, id: 'two', content: '第二条' }] });
    render(<Announcements blocked={false} />); await flush();
    expect(modal().open).toBe(true);
    const stored = JSON.parse(localStorage.getItem(STORAGE)!);
    expect(Object.keys(stored).sort()).toEqual(['one', 'two']);
    expect(localStorage.getItem(STORAGE)).not.toContain('第二条');
  });
  it('paginates full titles and multiple announcements and renders HTML as text', async () => {
    const title = '长标题'.repeat(70);
    request.mockResolvedValue({ announcements: [{ ...item, title }, { ...item, id: 'two', content: '<script>alert(1)</script>' }] });
    render(<Announcements blocked />); await flush(); open(); await flush();
    expect(localStorage.getItem(STORAGE)).toBeNull();
    let combined = document.querySelector('.announcements-text')!.textContent;
    while (!(screen.getByText('下一页') as HTMLButtonElement).disabled) {
      fireEvent.click(screen.getByText('下一页')); await flush(); combined += document.querySelector('.announcements-text')!.textContent!;
    }
    expect(combined).toBe(`${title}\n\n${item.content}`);
    expect(JSON.parse(localStorage.getItem(STORAGE)!)['one']).toBeTruthy();
    fireEvent.click(screen.getByText('下一条')); await flush();
    expect(document.querySelector('.announcements-text')!.textContent).toContain('<script>alert(1)</script>');
    expect(modal().querySelector('script')).toBeNull();
    fireEvent.click(screen.getByText('上一条')); await flush(); expect((screen.getByText('上一页') as HTMLButtonElement).disabled).toBe(true);
  });
  it('shows real failure/loading/empty states and rejects malformed responses', async () => {
    request.mockRejectedValueOnce(new Error('private diagnostic'));
    render(<Announcements blocked />); await flush(); open();
    expect(screen.getByText('公告获取失败，无法确认最新公告。')).toBeTruthy(); expect(screen.queryByText(/private diagnostic/)).toBeNull();
    request.mockResolvedValueOnce({ wrong: [] }); fireEvent.click(screen.getByText('重试')); await flush();
    expect(screen.getByText('公告获取失败，无法确认最新公告。')).toBeTruthy();
    request.mockResolvedValueOnce({ announcements: [] }); fireEvent.click(screen.getByText('重试'));
    expect(screen.getByText('正在获取公告…')).toBeTruthy(); await flush(); expect(screen.getByText('暂无公告')).toBeTruthy();
  });
  it('opens from idle button focus after unblocking', async () => {
    const button = document.createElement('button'); document.body.append(button); button.focus();
    const view = render(<Announcements blocked />); await flush(); expect(modal().open).toBe(false);
    view.rerender(<Announcements blocked={false} />); await flush(); expect(modal().open).toBe(true);
    view.unmount(); button.remove();
  });
  it('filters unix-second expirations between polls and normalizes numeric ids', async () => {
    vi.useFakeTimers();
    const seconds = Math.floor(Date.now() / 1000);
    request.mockResolvedValue({ announcements: [{ ...item, id: 42, expires_at: String(seconds + 2) }, { ...item, id: 'old', expires_at: seconds - 1 }] });
    render(<Announcements blocked />); await flush(); open(); await flush();
    expect(screen.getByText('第 1 / 1 条 · 通知')).toBeTruthy();
    expect(JSON.parse(localStorage.getItem(STORAGE)!)['42']).toBeTruthy();
    await act(async () => { vi.advanceTimersByTime(2001); });
    expect(screen.getByText('暂无公告')).toBeTruthy(); expect(request).toHaveBeenCalledTimes(1);
  });
  it('does not immediately reopen a dismissed unread long announcement', async () => {
    request.mockResolvedValue({ announcements: [{ ...item, content: '长内容'.repeat(200) }] });
    render(<Announcements blocked={false} />); await flush(); expect(modal().open).toBe(true);
    fireEvent.click(screen.getByText('关闭')); await flush(); expect(modal().open).toBe(false);
    expect(localStorage.getItem(STORAGE)).toBeNull();
    open(); await flush(); expect(modal().open).toBe(true);
  });
  it('handles disabled localStorage', async () => {
    vi.spyOn(localStorage, 'getItem').mockImplementation(() => { throw new Error('denied'); });
    vi.spyOn(localStorage, 'setItem').mockImplementation(() => { throw new Error('quota'); });
    render(<Announcements blocked={false} />); await flush(); expect(modal().open).toBe(true);
  });
});






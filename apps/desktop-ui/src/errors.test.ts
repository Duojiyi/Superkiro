import { describe, expect, it, vi, beforeEach, afterEach } from 'vitest';
import { ClientError, feedbackText, toClientError } from './errors';
const { invoke, isTauri } = vi.hoisted(() => ({ invoke: vi.fn(), isTauri: vi.fn(() => true) }));
vi.mock('@tauri-apps/api/core', () => ({ invoke, isTauri }));
import { api, native } from './bridge';
describe('structured client errors', () => {
  beforeEach(() => { invoke.mockReset(); isTauri.mockReturnValue(true); });
  afterEach(() => vi.useRealTimers());
  it('preserves feedback IDs, time and partial outcomes', () => {
    const error = toClientError({code:'SK-BIND-004', feedback_id:'HOST-12345678', stage:'unbind', outcome:'partial', occurred_at:'1789950000'});
    expect(error).toBeInstanceOf(ClientError);
    expect(error.feedback_id).toBe('HOST-12345678');
    expect(error.outcome).toBe('partial');
    expect(error.message).toContain('无需重复解绑');
    expect(error.occurred_at).toBe(new Date(1789950000000).toISOString());
    expect(toClientError(error)).toBe(error);
  });
  it('does not copy raw errors, paths, addresses or version garbage', () => {
    const error = toClientError({code:'secret-token', feedback_id:'/Users/secret', stage:'secret', occurred_at:'secret', retry_after_seconds:999999, message:'secret'});
    expect(feedbackText(error, {kiro_version:'https://secret', app_version:'secret', gateway_url:'https://secret', kiro_install_path:'/Users/secret'})).not.toContain('secret');
    expect(error.code).toBe('SK-UNKNOWN-001');
    expect(error.retry_after_seconds).toBeNull();
    expect(toClientError(new Error('Bearer secret')).message).not.toContain('secret');
    expect(toClientError('secret').feedback_id).not.toBe(error.feedback_id);
  });
  it('bounds cooldown and accepts release versions', () => {
    const error = toClientError({code:'SK-BIND-002', retry_after_seconds:60});
    expect(error.message).toContain('60 秒');
    expect(feedbackText(error, {app_version:'0.1.0-preview.abc', kiro_version:'1.1.14'})).toContain('0.1.0-preview.abc');
  });
  it('normalizes IPC rejections', async () => {
    invoke.mockRejectedValue({code:'SK-AUTH-003', feedback_id:'HOST-12345678'});
    await expect(api('/api/usage')).rejects.toMatchObject({code:'SK-AUTH-003', feedback_id:'HOST-12345678'});
    invoke.mockRejectedValue('private credentials');
    await expect(native('credentials_load')).rejects.toMatchObject({code:'SK-UNKNOWN-001'});
  });
  it.each([['POST',125000,'unknown'], ['GET',15000,'failed']])('handles %s timeout outcome', async (method, delay, outcome) => {
    vi.useFakeTimers(); invoke.mockReturnValue(new Promise(() => {}));
    const result = expect(api('/api/status', String(method))).rejects.toMatchObject({code:'SK-NET-001', outcome});
    await vi.advanceTimersByTimeAsync(Number(delay)); await result;
  });
  it('gives takeover, restore and unbind the time a save prompt needs', async () => {
    vi.useFakeTimers(); invoke.mockReturnValue(new Promise(() => {}));
    let settled = false; const result = api('/api/restore', 'POST').catch(e => { settled = true; return e; });
    await vi.advanceTimersByTimeAsync(125000); expect(settled).toBe(false);
    await vi.advanceTimersByTimeAsync(55000); expect(await result).toMatchObject({code:'SK-NET-001', outcome:'unknown'});
    vi.useRealTimers();
  });
  it('handles structured unsuccessful responses and native credential errors', async () => {
    invoke.mockResolvedValue({success:false,error:{code:'SK-BIND-002',retry_after_seconds:30,feedback_id:'HOST-12345678'}});
    await expect(api('/api/verify-card','POST')).rejects.toMatchObject({code:'SK-BIND-002',retry_after_seconds:30,feedback_id:'HOST-12345678'});
    invoke.mockRejectedValue({code:'SK-LOCAL-003',stage:'native'});
    await expect(native('get_remembered_card')).rejects.toMatchObject({code:'SK-LOCAL-003',stage:'native'});
  });
  it('preview never invokes native commands', async () => {
    isTauri.mockReturnValue(false);
    await expect(api('/api/activate','POST')).rejects.toMatchObject({code:'SK-PREVIEW-001'});
    expect(invoke).not.toHaveBeenCalled();
  });
});

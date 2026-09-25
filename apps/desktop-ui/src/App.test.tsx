import { listen } from '@tauri-apps/api/event';
// @vitest-environment jsdom
import { afterEach, describe, expect, it, vi } from 'vitest';
import { act, cleanup, fireEvent, render, screen, waitFor, within } from '@testing-library/react';
import { App } from './App';
import * as errors from './errors';
import { configured, expired, gateway, report, safeError, sanitizeChecks, operationFailureSummary, maintenanceText } from './bridge';
const invoke=vi.hoisted(()=>vi.fn());
function supportError(code:string,outcome:'failed'|'partial'|'unknown'='failed'){return {code,outcome,feedback_id:'test-feedback-123',stage:'unknown',retry_after_seconds:null,occurred_at:'2026-09-21T00:00:00Z'};}
vi.mock('@tauri-apps/api/event',()=>({listen:vi.fn(async()=>()=>{})}));
vi.mock('@tauri-apps/api/core',()=>({invoke,isTauri:()=>true}));
afterEach(()=>{cleanup();vi.clearAllMocks();});
function setup(installed=false,recovery=false){invoke.mockImplementation(async(command:string,payload:{method:string;path?:string})=>{
 if(command==='native'){if(payload.method==='get_remembered_card')return null;if(payload.method==='get_close_behavior')return 'tray';return true;}
 if(payload.path==='/api/operation')return {id:0,state:'idle'};
 if(payload.path==='/api/status')return {kiro_installed:installed,process_state:installed?'Running':'NotRunning',platform:'win32',kiro_compatible:installed ? true : undefined,minimum_kiro_version:'1.1.14',authenticated:false,has_snapshot:false,recovery_pending:recovery};
 if(payload.path==='/api/verify-card')return {success:true,authorization:{remainingPoints:100,totalPoints:100},gateway_url:'https://example.com'};
 if(payload.path==='/api/memory/sample')return {total_memory_mb:0,total_process_count:0};
 if(payload.path==='/api/usage')return {usage:{usageBreakdownList:[{dimensionType:'CREDIT',currentUsageWithPrecision:0,usageLimitWithPrecision:100}]}};
 return {success:true};
});}
async function login(){fireEvent.change(screen.getByLabelText('输入你的卡密'),{target:{value:'secret-card'}});fireEvent.click(screen.getByRole('button',{name:'登录 →'}));await screen.findByRole('navigation');}
async function connect(){
 HTMLDialogElement.prototype.showModal=function(){this.open=true;};
 const original=invoke.getMockImplementation()!;let connected=false;
 invoke.mockImplementation(async(command,payload)=>{const result=await original(command,payload);if(payload.path==='/api/activate')connected=true;if(payload.path==='/api/restore')connected=false;return payload.path==='/api/status'?{...result,authenticated:connected,has_snapshot:connected}:result;});
 fireEvent.click(screen.getByRole('button',{name:'启用连接'}));fireEvent.submit(document.querySelector('dialog form')!);
 await screen.findByRole('button',{name:'打开 Kiro ↗'});
}
describe('safe contract',()=>{
 it('does not conflate configured and unverified service',()=>{expect(configured({authenticated:true,has_snapshot:false})).toBe(false);expect(configured({authenticated:true,has_snapshot:true})).toBe(true);});
 it('expiry and gateway validation',()=>{expect(expired({validUntil:1})).toBe(true);expect(gateway('')).toBe('');expect(gateway('https://example.com/')).toBe('https://example.com');expect(()=>gateway('http://example.com')).toThrow();expect(()=>gateway('https://user:secret@example.com')).toThrow();});
 it('redacts diagnostic values and raw errors',()=>{const checks=sanitizeChecks([{name:'secret-key /Users/me',level:'secret'}]);expect(report(checks,'',{})).not.toContain('secret');expect(safeError('secret-key 1.2.3.4')).not.toContain('secret');});
});
describe('React states',()=>{
 it('login verifies without activating; no Kiro means no trim or fake samples',async()=>{setup();render(<App/>);await login();expect(screen.getByRole('heading',{name:'未找到 Kiro'})).toBeTruthy();expect(invoke.mock.calls.some(([,p])=>p.path==='/api/activate')).toBe(false);fireEvent.click(screen.getByRole('button',{name:/设置/}));await screen.findByText('Kiro 未运行');expect((screen.getByRole('button',{name:'立即整理'}) as HTMLButtonElement).disabled).toBe(true);expect(invoke.mock.calls.some(([,p])=>p.path==='/api/memory/sample'||p.path==='/api/memory/trim')).toBe(false);expect(document.querySelectorAll('.spark i')).toHaveLength(0);});
 it('usage empty state is not an error',async()=>{setup(true);render(<App/>);await login();await connect();fireEvent.click(screen.getByRole('button',{name:'刷新 ↻'}));await waitFor(()=>expect(document.querySelector('.pending-indicator')).toBeNull());expect(screen.getByText('今日已用积分')).toBeTruthy();expect(screen.queryByText(/用量刷新失败/)).toBeNull();expect(screen.getByText('— 积分')).toBeTruthy();});
 it('verification errors stay on login and redact raw payloads',async()=>{setup();const original=invoke.getMockImplementation()!;invoke.mockImplementation((command,payload)=>payload.path==='/api/verify-card'?Promise.reject({...supportError('SK-AUTH-001'),message:'secret upstream token'}):original(command,payload));render(<App/>);await waitFor(()=>expect((screen.getByLabelText('记住卡密') as HTMLInputElement).disabled).toBe(false));fireEvent.change(screen.getByLabelText('输入你的卡密'),{target:{value:'secret-card'}});fireEvent.click(screen.getByRole('button',{name:'登录 →'}));await screen.findByRole('button',{name:'重新登录 →'});expect(screen.getByRole('alert').textContent).not.toContain('secret');expect(screen.queryByRole('navigation')).toBeNull();});
 it('blocks activation for an older Kiro without sending activate',async()=>{setup(true);const original=invoke.getMockImplementation()!;invoke.mockImplementation((command,payload)=>payload.path==='/api/status'?original(command,payload).then((v:any)=>({...v,kiro_compatible:false,minimum_kiro_version:'1.1.14'})):original(command,payload));render(<App/>);await login();fireEvent.click(screen.getByRole('button',{name:'启用连接'}));await screen.findByRole('heading',{name:'需要升级 Kiro'});expect(screen.getByText(/1.1.14/)).toBeTruthy();expect(invoke.mock.calls.some(([,p])=>p.path==='/api/activate')).toBe(false);});
 it('blocks activation when Kiro compatibility is unknown',async()=>{setup(true);const original=invoke.getMockImplementation()!;invoke.mockImplementation((command,payload)=>payload.path==='/api/status'?original(command,payload).then((v:any)=>({...v,kiro_compatible:undefined})):original(command,payload));render(<App/>);await login();fireEvent.click(screen.getByRole('button',{name:'启用连接'}));await screen.findByRole('heading',{name:'无法确认 Kiro 版本'});expect(invoke.mock.calls.some(([,p])=>p.path==='/api/activate')).toBe(false);});
 it('keeps activation failures on the overview with a safe message',async()=>{setup(true);const original=invoke.getMockImplementation()!;invoke.mockImplementation((command,payload)=>payload.path==='/api/activate'?Promise.reject(supportError('SK-CONNECT-003','failed')):original(command,payload));render(<App/>);await login();fireEvent.click(screen.getByRole('button',{name:'启用连接'}));fireEvent.submit(document.querySelector('dialog form')!);await screen.findAllByText(/连接配置应用失败/);expect(screen.getByRole('heading',{name:'连接你的 Kiro'})).toBeTruthy();expect(screen.queryByRole('heading',{name:'连接诊断'})).toBeNull();});
 it('mutation rejection reports failure, never ready',async()=>{setup(true);HTMLDialogElement.prototype.showModal=function(){this.open=true;};const original=invoke.getMockImplementation()!;invoke.mockImplementation((command,payload)=>payload.path==='/api/activate'?Promise.reject(supportError('SK-NET-001','unknown')):original(command,payload));render(<App/>);await login();fireEvent.click(screen.getByRole('button',{name:'启用连接'}));expect(document.activeElement?.textContent).toBe('取消');fireEvent.submit(document.querySelector('dialog form')!);await screen.findByRole('heading',{name:'连接你的 Kiro'});expect(screen.queryByText('Kiro 已就绪')).toBeNull();expect(screen.queryByRole('button',{name:'修复并重新连接'})).toBeNull();expect((screen.getByRole('button',{name:'启用连接'}) as HTMLButtonElement).disabled).toBe(true);await waitFor(()=>expect(screen.getByRole('status').textContent).toContain('超时'));});
});


describe('tray lifecycle',()=>{
 it('window close hides without restoration when tray is available',async()=>{setup(true);const original=invoke.getMockImplementation()!;invoke.mockImplementation(async(command,payload)=>{const result=await original(command,payload);return payload.path==='/api/status'?{...result,tray_available:true}:result;});render(<App/>);await login();fireEvent.click(screen.getByRole('button',{name:'关闭窗口'}));await waitFor(()=>expect(invoke).toHaveBeenCalledWith('native',{method:'close',args:[]}));expect(invoke.mock.calls.some(([,p])=>p.path==='/api/restore')).toBe(false);});
 it('explicit exit restores before native exit, not close',async()=>{setup(true);HTMLDialogElement.prototype.showModal=function(){this.open=true;};render(<App/>);await login();fireEvent.click(screen.getByRole('button',{name:/设置/}));fireEvent.click(screen.getByRole('button',{name:'退出程序'}));fireEvent.submit(document.querySelector('dialog form')!);await waitFor(()=>expect(invoke).toHaveBeenCalledWith('native',{method:'exit',args:[]}));expect(invoke.mock.calls.findIndex(([,p])=>p.path==='/api/restore')).toBeLessThan(invoke.mock.calls.findIndex(([,p])=>p.method==='exit'));});
});

describe('real maintenance status',()=>{
 it('missing status stays unknown',()=>expect(maintenanceText(undefined).label).toBe('维护状态未知'));
 it('enabled is not successful optimization',()=>{const value=maintenanceText({enabled:true,mode:'automatic',threshold_mb:2500,cooldown_seconds:300,last_sample_mb:null,last_trim:null});expect(value.label).toBe('自动维护已开启');expect(value.detail).toContain('尚无已确认');expect(value.detail).toContain('2,500');});
 it('monitor-only is not automatic trim',()=>expect(maintenanceText({enabled:true,mode:'monitor-only'}).label).toBe('仅监测，不自动整理'));
 it('redacts raw maintenance errors and does not claim success',()=>{const value=maintenanceText({enabled:true,mode:'automatic',last_error:'secret gateway 1.2.3.4',last_trim:{success_count:1,failed_count:0}});expect(value.detail).not.toContain('secret');expect(value.detail).toContain('未确认优化成功');});
 it('only actual nonzero successes are reported as historical results',()=>{expect(maintenanceText({last_trim:{success_count:0,failed_count:0}}).detail).toContain('尚无已确认');expect(maintenanceText({last_trim:{success_count:2,failed_count:1}}).detail).toContain('最近一次整理：2 成功，1 失败');});
 it('automatic maintenance with no Kiro does not fake a curve or trim',async()=>{setup(false);const original=invoke.getMockImplementation()!;invoke.mockImplementation(async(command,payload)=>{const result=await original(command,payload);return payload.path==='/api/status'?{...result,memory_maintenance:{enabled:true,mode:'automatic',threshold_mb:2500,cooldown_seconds:300,last_sample_mb:null,last_trim:null}}:result;});render(<App/>);await login();fireEvent.click(screen.getByRole('button',{name:/设置/}));await screen.findByText('自动维护已开启');await screen.findByText('Kiro 未运行');expect(document.querySelectorAll('.spark i')).toHaveLength(0);expect((screen.getByRole('button',{name:'立即整理'}) as HTMLButtonElement).disabled).toBe(true);});
});

describe('recovery and stale usage regressions',()=>{
 it.each([{has_snapshot:true,authenticated:false},{has_snapshot:false,authenticated:false,recovery_pending:true}])('opens recovery without a token or card: %j',async(recovery)=>{setup(false);HTMLDialogElement.prototype.showModal=function(){this.open=true;};const original=invoke.getMockImplementation()!;let restored=false;invoke.mockImplementation(async(command,payload)=>{if(payload.path==='/api/restore'){restored=true;return {success:true};}const result=await original(command,payload);return payload.path==='/api/status'&&!restored?{...result,...recovery}:result;});render(<App/>);await screen.findByRole('heading',{name:'本机配置待恢复'});expect(screen.queryByLabelText('输入你的卡密')).toBeNull();expect(screen.queryByRole('button',{name:'修复并重新连接'})).toBeNull();expect(screen.queryByRole('button',{name:'启用连接'})).toBeNull();fireEvent.click(screen.getByRole('button',{name:'还原 Kiro 配置'}));fireEvent.submit(document.querySelector('dialog form')!);await screen.findByRole('heading',{name:'卡密登录'});expect(invoke.mock.calls.some(([,p])=>p.path==='/api/verify-card')).toBe(false);expect(invoke).toHaveBeenCalledWith('api',{path:'/api/restore',method:'POST',body:{close_kiro_confirmed:true}});});
 it('startup recovery opens overview without a card',async()=>{setup(false,true);render(<App/>);await screen.findByRole('heading',{name:'本机配置待恢复'});expect(screen.getByRole('button',{name:'还原 Kiro 配置'})).toBeTruthy();});
 it('failed usage refresh preserves confirmed balance but hides daily values',async()=>{setup(true);const original=invoke.getMockImplementation()!;let fail=false;invoke.mockImplementation(async(command,payload)=>{if(payload.path==='/api/usage'){if(fail)throw supportError('SK-NET-002');return {usage:{availableCredits:977,usageBreakdownList:[{dimensionType:'CREDIT',currentUsageWithPrecision:10,usageLimitWithPrecision:987}]},settledUsage:{todayPoints:123,todayTokens:456}};}const result=await original(command,payload);return payload.path==='/api/status'?{...result,authenticated:true,has_snapshot:true,recovery_pending:true}:result;});render(<App/>);await screen.findByText('977');await screen.findByText('123 积分');fail=true;fireEvent.click(screen.getByRole('button',{name:'刷新 ↻'}));await screen.findByText('用量刷新失败，保留最近确认余额；今日用量暂不可用。');fireEvent.click(screen.getByRole('button',{name:/概览/}));expect(screen.getByText('用量刷新失败，保留最近确认余额；今日用量暂不可用。')).toBeTruthy();expect(screen.getByText('977')).toBeTruthy();expect(screen.queryByText('123 积分')).toBeNull();expect(screen.queryByText('456')).toBeNull();});
});

describe('confirmed restoration before exit',()=>{
 it('asks for a Kiro reinstall when the modified extension cannot be rolled back',async()=>{setup(true);const original=invoke.getMockImplementation()!;invoke.mockImplementation(async(command,payload)=>{const result=await original(command,payload);return payload.path==='/api/status'?{...result,recovery_pending:true,recovery_blocked:'reinstall_kiro'}:result;});render(<App/>);await screen.findByRole('heading',{name:'本机配置待恢复'});expect(screen.getByText(/备份已丢失/)).toBeTruthy();expect(screen.getByRole('button',{name:'还原 Kiro 配置'})).toBeTruthy();fireEvent.click(screen.getByRole('button',{name:'下载 Kiro ↗'}));await waitFor(()=>expect(invoke).toHaveBeenCalledWith('native',{method:'open_external',args:['https://kiro.dev/downloads/']}));});
 it('shows no reinstall guidance for an ordinary recovery',async()=>{setup(true);const original=invoke.getMockImplementation()!;invoke.mockImplementation(async(command,payload)=>{const result=await original(command,payload);return payload.path==='/api/status'?{...result,recovery_pending:true}:result;});render(<App/>);await screen.findByRole('heading',{name:'本机配置待恢复'});expect(screen.queryByText(/备份已丢失/)).toBeNull();expect(screen.queryByRole('button',{name:'下载 Kiro ↗'})).toBeNull();});
 it('does not exit when status still has recovery artifacts after successful restore',async()=>{setup(true);HTMLDialogElement.prototype.showModal=function(){this.open=true;};const original=invoke.getMockImplementation()!;invoke.mockImplementation(async(command,payload)=>{const result=await original(command,payload);return payload.path==='/api/status'?{...result,recovery_pending:true}:result;});render(<App/>);await screen.findByRole('heading',{name:'本机配置待恢复'});fireEvent.click(screen.getByRole('button',{name:'关闭窗口'}));fireEvent.submit(document.querySelector('dialog form')!);await screen.findByRole('heading',{name:'恢复未完成'});expect(invoke.mock.calls.some(([,p])=>p.method==='exit')).toBe(false);});
});


describe('desktop tray event',()=>{
 it('a remembered or typed card never appears in the page markup',async()=>{setup();const original=invoke.getMockImplementation()!;invoke.mockImplementation(async(command,payload)=>command==='native'&&payload.method==='get_remembered_card'?'REMEMBERED-CARD-9876':original(command,payload));render(<App/>);const field=await screen.findByLabelText('输入你的卡密') as HTMLInputElement;await waitFor(()=>expect(field.value).toBe('REMEMBERED-CARD-9876'));expect(document.body.outerHTML).not.toContain('REMEMBERED-CARD-9876');fireEvent.change(field,{target:{value:'TYPED-CARD-5432'}});expect(field.value).toBe('TYPED-CARD-5432');expect(document.body.outerHTML).not.toContain('TYPED-CARD-5432');});
 it('an exit requested from the tray describes what is configured now',async()=>{setup(true);HTMLDialogElement.prototype.showModal=function(){this.open=true;};render(<App/>);await screen.findByRole('heading',{name:'卡密登录'});await waitFor(()=>expect(invoke.mock.calls.some(([,p])=>p.path==='/api/status')).toBe(true));await act(async()=>{await Promise.resolve();});const subscriptions=vi.mocked(listen).mock.calls.filter(call=>call[0]==='desktop-exit-request');expect(subscriptions).toHaveLength(1);subscriptions[0][1]({event:'desktop-exit-request',id:1,payload:null});const heading=await screen.findByRole('heading',{name:'退出 Superkiro？'});expect(heading.closest('dialog')!.textContent).toContain('当前没有待还原的本机配置');});
 it('an unbind after a verification-only login does not claim a restore',async()=>{setup(true);HTMLDialogElement.prototype.showModal=function(){this.open=true;};render(<App/>);await login();fireEvent.click(screen.getByRole('button',{name:/设置/}));fireEvent.click(screen.getByRole('button',{name:'解除设备绑定'}));fireEvent.change(screen.getByLabelText('当前卡密'),{target:{value:'secret-card'}});fireEvent.submit(document.querySelector('dialog form')!);await screen.findByText('设备绑定已解除。');expect(screen.queryByText(/Kiro 配置已还原/)).toBeNull();});
 it('desktop-exit-request opens restoration confirmation',async()=>{setup(true);HTMLDialogElement.prototype.showModal=function(){this.open=true;};render(<App/>);await login();const subscription=vi.mocked(listen).mock.calls.filter(call=>call[0]==='desktop-exit-request').at(-1)!;expect(subscription).toBeTruthy();subscription[1]({event:'desktop-exit-request',id:1,payload:null});await screen.findByRole('heading',{name:'退出 Superkiro？'});expect(invoke.mock.calls.some(([,p])=>p.method==='exit')).toBe(false);});
});

describe('billing time zone',()=>{
 it('labels UTC day totals without using local today',async()=>{setup(true);const original=invoke.getMockImplementation()!;invoke.mockImplementation(async(command,payload)=>{if(payload.path==='/api/usage')return {usage:{usageBreakdownList:[{dimensionType:'CREDIT',currentUsageWithPrecision:1,usageLimitWithPrecision:100}]},settledUsage:{timezone:'UTC',windowStart:'2026-09-19T00:00:00Z',windowEnd:'2026-09-20T00:00:00Z',todayPoints:1,todayTokens:2}};const result=await original(command,payload);return payload.path==='/api/status'?{...result,authenticated:true,has_snapshot:true}:result;});render(<App/>);await screen.findByText('1 积分');expect(screen.getByText('今日已用积分')).toBeTruthy();expect(screen.getByTitle(/UTC/).getAttribute('title')).toContain('已结算');expect(document.body.textContent).not.toMatch(/tokens/i);});
});


describe('motion follows real pending work',()=>{
 it('keeps login pending until verification settles, then clears on failure',async()=>{
  setup();const original=invoke.getMockImplementation()!;
  let reject!:(reason:unknown)=>void;
  invoke.mockImplementation((command,payload)=>payload.path==='/api/verify-card'?new Promise((_,fail)=>{reject=fail;}):original(command,payload));
  render(<App/>);
  expect(document.querySelector('.wave')?.getAttribute('aria-hidden')).toBe('true');
  expect(document.querySelector('.pending-indicator')).toBeNull();
  fireEvent.change(screen.getByLabelText('输入你的卡密'),{target:{value:'secret-card'}});
  fireEvent.click(screen.getByRole('button',{name:'登录 →'}));
  await screen.findByRole('button',{name:'正在验证…'});
  expect(document.querySelector('.wave.responding')).not.toBeNull();
  expect(document.querySelector('.shell')?.getAttribute('aria-busy')).toBe('true');
  expect(document.querySelector('.pending-indicator')?.textContent).toBe('正在处理');
  reject(supportError('SK-NET-002'));
  await screen.findByRole('button',{name:'重新登录 →'});
  expect(document.querySelector('.pending-indicator')).toBeNull();
  expect(document.querySelector('.wave.responding')).toBeNull();
  expect(document.querySelector('.shell')?.getAttribute('aria-busy')).toBe('false');
 });
 it.each(['usage','memory'] as const)('shows %s loading only while the request is pending, without synthetic samples',async(kind)=>{
  setup(true);const original=invoke.getMockImplementation()!;
  let reject!:(reason:unknown)=>void;
  const path=kind==='usage'?'/api/usage':'/api/memory/sample';
  invoke.mockImplementation((command,payload)=>payload.path===path?new Promise((_,fail)=>{reject=fail;}):original(command,payload));
  render(<App/>);await login();
  if(kind==='usage')await connect();
  fireEvent.click(screen.getByRole('button',{name:kind==='usage'?'刷新 ↻':'设置'}));
  await waitFor(()=>expect(document.querySelector('.pending-indicator')?.textContent).toBe(kind==='usage'?'正在读取用量':'正在采样'));
  await waitFor(()=>expect(reject).toBeTypeOf('function'));
  expect(document.querySelectorAll('.spark i')).toHaveLength(0);
  reject(supportError('SK-NET-002'));
  await screen.findByText(kind==='usage'?'用量刷新失败，保留最近确认余额；今日用量暂不可用。':'采样失败，当前占用与维护结果未确认');
  expect(document.querySelector('.pending-indicator')).toBeNull();
  expect(document.querySelectorAll('.spark i')).toHaveLength(0);
 });
});


describe('native titlebar drag regions',()=>{
 it('drags from header and top padding but excludes controls and page content',async()=>{
  setup(true);render(<App/>);
  const checkDrag=()=>{
   const header=document.querySelector('header')!;
   vi.spyOn(header,'getBoundingClientRect').mockReturnValue({bottom:72} as DOMRect);
   for(const selector of ['header','.brand','.window-drag','.shell']){
    invoke.mockClear();fireEvent.mouseDown(document.querySelector(selector)!,{button:0,clientY:20});
    expect(invoke).toHaveBeenCalledWith('native',{method:'drag',args:[]});
   }
   for(const control of document.querySelectorAll('button,input,a,select,textarea')){
    invoke.mockClear();fireEvent.mouseDown(control,{button:0,clientY:20});
    expect(invoke.mock.calls.some(([,p])=>p?.method==='drag')).toBe(false);
   }
   invoke.mockClear();fireEvent.mouseDown(document.querySelector('.shell')!,{button:0,clientY:100});
   fireEvent.mouseDown(header,{button:2,clientY:20});
   expect(invoke.mock.calls.some(([,p])=>p?.method==='drag')).toBe(false);
  };
  checkDrag();await login();checkDrag();
  fireEvent.click(screen.getByRole('button',{name:'最小化'}));
  await waitFor(()=>expect(invoke).toHaveBeenCalledWith('native',{method:'minimize',args:[]}));
  await waitFor(()=>expect(document.querySelector('.shell')?.getAttribute('aria-busy')).toBe('false'));
  HTMLDialogElement.prototype.showModal=function(){this.open=true;};
  fireEvent.click(screen.getByRole('button',{name:'关闭窗口'}));
  await screen.findByRole('heading',{name:'退出 Superkiro？'});
 });
 it('preserves native size across main tabs and supports Windows header double click',async()=>{
  setup(true);render(<App/>);await login();
  invoke.mockClear();
  fireEvent.click(screen.getByRole('button',{name:'设置'}));
  await screen.findByRole('heading',{name:'设置'});
  fireEvent.click(screen.getByRole('button',{name:'概览'}));
  fireEvent.click(screen.getByRole('button',{name:'设置'}));
  expect(invoke.mock.calls.some(([,p])=>p?.method==='screen')).toBe(false);
  fireEvent.mouseDown(document.querySelector('header')!,{button:0,detail:2,clientY:0});
  expect(invoke).toHaveBeenCalledWith('native',{method:'maximize',args:[]});
  expect(invoke.mock.calls.some(([,p])=>p?.method==='drag')).toBe(false);
 });
 it('keeps the busy indicator and its text draggable during a pending operation',async()=>{
  setup();const original=invoke.getMockImplementation()!;
  let reject!:(reason:unknown)=>void;
  invoke.mockImplementation((command,payload)=>payload.path==='/api/verify-card'?new Promise((_,fail)=>{reject=fail;}):original(command,payload));
  render(<App/>);
  fireEvent.change(screen.getByLabelText('输入你的卡密'),{target:{value:'secret-card'}});
  fireEvent.click(screen.getByRole('button',{name:'登录 →'}));
  await screen.findByRole('button',{name:'正在验证…'});
  for(const selector of ['.pending-indicator','.pending-indicator .sr-only']){invoke.mockClear();fireEvent.mouseDown(document.querySelector(selector)!,{button:0,clientY:0});expect(invoke).toHaveBeenCalledWith('native',{method:'drag',args:[]});}
  reject(supportError('SK-NET-002'));await screen.findByRole('button',{name:'重新登录 →'});
 });
});


describe('constrained viewport and activation recovery',()=>{
 it('keeps startup notice and login retry in the scroll region without the removed footer',async()=>{
  setup();const original=invoke.getMockImplementation()!;
  invoke.mockImplementation((command,payload)=>['/api/status','/api/verify-card'].includes(payload.path)?Promise.reject(supportError('SK-NET-002')):original(command,payload));
  render(<App/>);
  await screen.findByRole('button',{name:'关闭提示'});
  fireEvent.change(screen.getByLabelText('输入你的卡密'),{target:{value:'secret-card'}});
  fireEvent.click(screen.getByRole('button',{name:'登录 →'}));
  await screen.findByRole('button',{name:'重新登录 →'});
  const region=screen.getByRole('region',{name:'页面内容'});
  expect(region.contains(screen.getByRole('button',{name:'关闭提示'}))).toBe(true);
  expect(region.contains(screen.getByRole('button',{name:'重新登录 →'}))).toBe(true);
  expect(region.firstElementChild?.classList.contains('notice')).toBe(true);
  expect(screen.queryByRole('button',{name:'无需卡密，查看诊断与恢复'})).toBeNull();
 });
 it.each(['recovery','unavailable','clean'] as const)('refreshes status after activation failure: %s',async(result)=>{
  setup(true);HTMLDialogElement.prototype.showModal=function(){this.open=true;};
  const original=invoke.getMockImplementation()!;let attempted=false;
  invoke.mockImplementation(async(command,payload)=>{
   if(payload.path==='/api/activate'){attempted=true;throw {...supportError('SK-CONNECT-003'),message:'write failed https://private.example/token'};}
   if(payload.path==='/api/status'&&attempted){
    if(result==='unavailable')throw supportError('SK-NET-002');
    return {kiro_installed:true,authenticated:false,has_snapshot:result==='recovery',recovery_pending:result==='recovery'};
   }
   return original(command,payload);
  });
  render(<App/>);await login();
  fireEvent.click(screen.getByRole('button',{name:'启用连接'}));fireEvent.submit(document.querySelector('dialog form')!);
  await screen.findByRole('heading',{name:result==='recovery'?'本机配置待恢复':'连接你的 Kiro'});
  await waitFor(()=>expect(document.querySelector('.shell')?.getAttribute('aria-busy')).toBe('false'));
  expect(screen.queryByRole('button',{name:'修复并重新连接'})).toBeNull();
  fireEvent.click(screen.getByRole('button',{name:'概览'}));
  if(result==='clean')expect((screen.getByRole('button',{name:'启用连接'}) as HTMLButtonElement).disabled).toBe(false);
  else if(result==='unavailable')expect((screen.getByRole('button',{name:'启用连接'}) as HTMLButtonElement).disabled).toBe(true);
  else expect(screen.queryByRole('button',{name:'启用连接'})).toBeNull();
  expect(invoke.mock.calls.filter(([,p])=>p.path==='/api/activate')).toHaveLength(1);
  expect(screen.getByRole('status').textContent).toContain('连接配置应用失败');
  expect(screen.getByRole('status').textContent).not.toContain('private.example');
  const activation=invoke.mock.calls.findIndex(([,p])=>p.path==='/api/activate');
  expect(invoke.mock.calls.slice(activation+1).some(([,p])=>p.path==='/api/status')).toBe(true);
  expect(screen.queryByText('连接配置已应用，请在 Kiro 中验证真实模型对话。')).toBeNull();
 });
});


describe('safe connection stage errors',()=>{
 it.each(['preflight','launch-prepare','authenticate','close','apply','launch'])('preserves only the approved %s stage, never raw details',stage=>{
  const raw=`[connection:${stage}] permission credential https://private.example/?key=secret C:/Users/private/file secret-card`;
  for(const error of [raw,new Error(raw)]){
   const message=safeError(error);
   expect(message).toContain(`[connection:${stage}]`);
   for(const secret of ['https://','private','secret','C:/','credential'])expect(message).not.toContain(secret);
  }
 });
 it('keeps timeout uncertainty alongside the stage',()=>{
  expect(safeError('[connection:authenticate] timeout secret')).toContain('操作结果未确认');
  expect(safeError('[connection:authenticate] timeout secret')).toContain('[connection:authenticate]');
 });
 it('does not echo unknown or embedded stage strings',()=>{
  expect(safeError('[connection:secret-stage] private')).not.toContain('secret-stage');
  expect(safeError('private [connection:apply]')).not.toContain('[connection:apply]');
 });
});


describe('audit timeout recovery',()=>{
 it.each(['succeeded','failed'])('unlocks only after a newer %s operation and status confirmation',async(state)=>{
  setup(true);HTMLDialogElement.prototype.showModal=function(){this.open=true;};
  const original=invoke.getMockImplementation()!;let operationCalls=0;
  invoke.mockImplementation((command,payload)=>{
   if(payload.path==='/api/operation')return Promise.resolve(++operationCalls<=2?{id:4,state:'idle'}:{id:5,state,support_error:{...supportError('SK-RESTORE-001'),feedback_id:'host-final-456'}});
   if(payload.path==='/api/activate')return Promise.reject(supportError('SK-NET-001','unknown'));
   return original(command,payload);
  });
  render(<App/>);await login();fireEvent.click(screen.getByRole('button',{name:'启用连接'}));fireEvent.submit(document.querySelector('dialog form')!);
  await screen.findByRole('heading',{name:'连接你的 Kiro'});
  fireEvent.click(screen.getByRole('button',{name:'概览'}));
  await waitFor(()=>expect((screen.getByRole('button',{name:'启用连接'}) as HTMLButtonElement).disabled).toBe(false));
  expect(screen.getByRole('status').textContent).toContain(state==='succeeded'?'宿主操作已':'本地配置还原未完成');
  if(state==='succeeded')expect(screen.queryByRole('region',{name:'错误反馈'})).toBeNull();
  if(state==='failed'){const card=screen.getByRole('region',{name:'错误反馈'});expect(card.textContent).toContain('host-final-456');expect(card.textContent).not.toContain('test-feedback-123');}
 });
 it.each(['succeeded','unknown'])('stops polling remote-unknown, permits only local restore and tracks its %s result',async restoreOutcome=>{
  setup(true);HTMLDialogElement.prototype.showModal=function(){this.open=true;};
  const original=invoke.getMockImplementation()!;let operationCalls=0;
  invoke.mockImplementation((c,p)=>{
   if(p.path==='/api/operation')return Promise.resolve(++operationCalls<=2?{id:4,state:'idle'}:{id:5,state:'failed',support_error:{...supportError('SK-NET-001','unknown'),feedback_id:'remote-final-456'}});
   if(p.path==='/api/activate')return Promise.reject(supportError('SK-NET-001','unknown'));
   if(p.path==='/api/restore')return restoreOutcome==='succeeded'?Promise.resolve({success:true}):Promise.reject({...supportError('SK-NET-001','unknown'),feedback_id:'restore-feedback-789'});
   return original(c,p);
  });
  render(<App/>);await login();fireEvent.click(screen.getByRole('button',{name:'启用连接'}));fireEvent.submit(document.querySelector('dialog form')!);
  await screen.findByText(/宿主操作已结束，但云端结果未确认/);
  expect(screen.getByRole('region',{name:'错误反馈'}).textContent).toContain('remote-final-456');
  expect((screen.getByRole('button',{name:'启用连接'}) as HTMLButtonElement).disabled).toBe(true);
  const calls=operationCalls;
  await act(async()=>{await new Promise(resolve=>setTimeout(resolve,2200));});
  expect(operationCalls).toBe(calls);
  fireEvent.click(screen.getByRole('button',{name:'关闭错误反馈'}));
  fireEvent.click(screen.getByRole('button',{name:'设置'}));fireEvent.change(screen.getByLabelText('设置分组'),{target:{value:'account'}});
  for(const action of ['解除设备绑定','切换卡密','退出程序']){
   fireEvent.click(screen.getByRole('button',{name:action}));expect(document.querySelector('dialog form')).toBeNull();
  }
  expect(invoke.mock.calls.some(([,p])=>p.path==='/api/unbind'||p.path==='/api/restore'||p.method==='exit')).toBe(false);
  fireEvent.click(screen.getByRole('button',{name:'仅还原本机配置'}));fireEvent.submit(document.querySelector('dialog form')!);
  if(restoreOutcome==='succeeded'){
   await screen.findByText('Kiro 配置已还原，当前卡密与积分信息已保留。');
   expect(screen.queryByRole('region',{name:'错误反馈'})).toBeNull();
  }else{
   await screen.findByText('反馈编号：restore-feedback-789');
   await waitFor(()=>expect(operationCalls).toBeGreaterThan(calls+1));
   fireEvent.click(screen.getByRole('button',{name:'退出程序'}));
   expect(document.querySelector('dialog form')).toBeNull();
   expect(screen.getByRole('heading',{name:'设置'})).toBeTruthy();
  }
  expect(screen.queryByRole('button',{name:'仅还原本机配置'})).toBeNull();
  expect(invoke.mock.calls.filter(([,p])=>p.path==='/api/restore')).toHaveLength(1);
  expect(invoke.mock.calls.some(([,p])=>p.path==='/api/unbind'||p.method==='exit')).toBe(false);
 });
 it.each(['preflight','race-running','race-terminal'])('waits for the tracked operation ID after %s lock contention',async kind=>{
  setup(true);HTMLDialogElement.prototype.showModal=function(){this.open=true;};
  const original=invoke.getMockImplementation()!;let operationCalls=0;
  invoke.mockImplementation((c,p)=>{
   if(p.path==='/api/operation'){operationCalls++;return Promise.resolve({id:operationCalls<=2||kind==='preflight'?8:9,state:operationCalls<=2?(kind==='preflight'?'running':'idle'):kind==='race-running'&&operationCalls===3?'running':'succeeded'});}
   if(p.path==='/api/activate')return Promise.reject(supportError('SK-LOCAL-002'));
   return original(c,p);
  });
  render(<App/>);await login();fireEvent.click(screen.getByRole('button',{name:'启用连接'}));fireEvent.submit(document.querySelector('dialog form')!);
  await screen.findByText(/宿主操作已结束/);
  expect((screen.getByRole('button',{name:'启用连接'}) as HTMLButtonElement).disabled).toBe(false);
  expect(invoke.mock.calls.filter(([,p])=>p.path==='/api/activate')).toHaveLength(kind==='preflight'?0:1);
  expect(screen.queryByRole('region',{name:'错误反馈'})).toBeNull();
 });
 it('does not reconcile an old terminal ID after an untracked lock rejection',async()=>{
  setup(true);HTMLDialogElement.prototype.showModal=function(){this.open=true;};
  const original=invoke.getMockImplementation()!;
  invoke.mockImplementation((c,p)=>p.path==='/api/operation'?Promise.resolve({id:8,state:'succeeded'}):p.path==='/api/activate'?Promise.reject(supportError('SK-LOCAL-002')):original(c,p));
  render(<App/>);await login();fireEvent.click(screen.getByRole('button',{name:'启用连接'}));fireEvent.submit(document.querySelector('dialog form')!);
  // Refused before it started: said as such, not as a failure or an unconfirmed outcome.
  await screen.findByText(/另一个操作正在进行，本次请求未执行/);
  expect((screen.getByRole('button',{name:'启用连接'}) as HTMLButtonElement).disabled).toBe(false);
  expect(screen.queryByRole('region',{name:'错误反馈'})).toBeNull();
  expect(screen.queryByText(/宿主操作已结束/)).toBeNull();
  // The startup read, the mutation baseline, and the contention check.
  expect(invoke.mock.calls.filter(([,p])=>p.path==='/api/operation')).toHaveLength(3);
 });
 it('keeps partial unbind protection after dismissal and navigation until local restore clears the session',async()=>{
  setup(true);const original=invoke.getMockImplementation()!;
  invoke.mockImplementation((c,p)=>p.path==='/api/unbind'?Promise.reject(supportError('SK-BIND-004','partial')):original(c,p));
  render(<App/>);await login();await connect();fireEvent.click(screen.getByRole('button',{name:'设置'}));
  fireEvent.change(screen.getByLabelText('设置分组'),{target:{value:'account'}});
  fireEvent.click(screen.getByRole('button',{name:'解除设备绑定'}));fireEvent.change(screen.getByLabelText('当前卡密'),{target:{value:'secret-card'}});fireEvent.submit(document.querySelector('dialog form')!);
  await screen.findByRole('heading',{name:'云端操作已完成，本机仍需处理'});
  fireEvent.click(screen.getByRole('button',{name:'关闭错误反馈'}));fireEvent.click(screen.getByRole('button',{name:'设置'}));
  fireEvent.click(screen.getByRole('button',{name:'解除设备绑定'}));expect(document.querySelector('dialog form')).toBeNull();
  expect(invoke.mock.calls.filter(([,p])=>p.path==='/api/unbind')).toHaveLength(1);
  fireEvent.click(screen.getByRole('button',{name:'仅还原本机配置'}));fireEvent.submit(document.querySelector('dialog form')!);
  await screen.findByRole('heading',{name:'卡密登录'});
  expect(screen.queryByRole('button',{name:'仅还原本机配置'})).toBeNull();
  expect(invoke.mock.calls.filter(([,p])=>p.path==='/api/restore')).toHaveLength(1);
  await login();fireEvent.click(screen.getByRole('button',{name:'设置'}));
  fireEvent.click(screen.getByRole('button',{name:'解除设备绑定'}));expect(document.querySelector('dialog form')).not.toBeNull();
 });
 it('clears usage errors when switching to a newly verified card',async()=>{
  setup(true);HTMLDialogElement.prototype.showModal=function(){this.open=true;};const original=invoke.getMockImplementation()!;
  invoke.mockImplementation((command,payload)=>payload.path==='/api/usage'?Promise.reject(supportError('SK-NET-002','failed')):original(command,payload));
  render(<App/>);await login();await connect();fireEvent.click(screen.getByRole('button',{name:'刷新 ↻'}));await screen.findByText('用量刷新失败，保留最近确认余额；今日用量暂不可用。');
  fireEvent.click(screen.getByRole('button',{name:/设置/}));fireEvent.click(screen.getByRole('button',{name:/切换卡密/}));fireEvent.submit(document.querySelector('dialog form')!);
  await screen.findByRole('button',{name:'登录 →'});await login();expect(screen.queryByText(/用量刷新失败/)).toBeNull();expect(document.querySelector('.balance-value')?.textContent).toBe('100');
 });
});


describe('cross-card in-flight usage isolation',()=>{
 it('ignores card A usage arriving after card B verification',async()=>{
  setup(true);HTMLDialogElement.prototype.showModal=function(){this.open=true;};const original=invoke.getMockImplementation()!;
  let resolveUsage!:(value:unknown)=>void;
  invoke.mockImplementation((command,payload)=>payload.path==='/api/usage'?new Promise(resolve=>{resolveUsage=resolve;}):original(command,payload));
  render(<App/>);await login();await connect();fireEvent.click(screen.getByRole('button',{name:'刷新 ↻'}));await screen.findByText('正在刷新用量，余额仍为最近确认值。');
  fireEvent.click(screen.getByRole('button',{name:/设置/}));fireEvent.click(screen.getByRole('button',{name:/切换卡密/}));fireEvent.submit(document.querySelector('dialog form')!);
  await screen.findByRole('button',{name:'登录 →'});await login();
  resolveUsage({usage:{availableCredits:777,usageBreakdownList:[{dimensionType:'CREDIT',currentUsageWithPrecision:99,usageLimitWithPrecision:100}]}});
  await waitFor(()=>expect(document.querySelector('.balance-value')?.textContent).toBe('100'));
 });
});

it('explains unsupported renamed macOS bundles without exposing paths',()=>{expect(safeError('MacBundleNameUnsupported /Users/private/Renamed.app')).toBe('macOS 暂仅支持保留官方包名 Kiro.app 的安装，请恢复官方包名后重新选择。');});

describe('platform-specific installation and minimal login',()=>{
 it('never exposes gateway controls or URLs on card login',async()=>{setup();render(<App/>);await screen.findByRole('heading',{name:'卡密登录'});expect(document.querySelector('input[type=url]')).toBeNull();expect(screen.queryByText(/https?:\/\//)).toBeNull();expect(screen.queryByLabelText(/网关/)).toBeNull();});
 it.each([['darwin','选择 Kiro.app'],['win32','选择 Kiro 安装文件夹']])('names the %s installation picker',async(platform,label)=>{
  setup();const original=invoke.getMockImplementation()!;invoke.mockImplementation(async(command,payload)=>{const result=await original(command,payload);return payload.path==='/api/status'?{...result,platform}:result;});
  render(<App/>);await login();expect(screen.getByRole('button',{name:label})).toBeTruthy();
 });
});


describe('review preflight and reserved balance',()=>{
 it('does not lock mutations when baseline and status fail before POST',async()=>{
  setup(true);HTMLDialogElement.prototype.showModal=function(){this.open=true;};const original=invoke.getMockImplementation()!;let preflightFailed=false;
  invoke.mockImplementation((command,payload)=>{if(payload.path==='/api/operation'){preflightFailed=true;return Promise.reject(supportError('SK-NET-002','failed'));}if(payload.path==='/api/status'&&preflightFailed)return Promise.reject(supportError('SK-NET-002','failed'));return original(command,payload);});
  render(<App/>);await login();fireEvent.click(screen.getByRole('button',{name:'启用连接'}));fireEvent.submit(document.querySelector('dialog form')!);
  await screen.findByRole('heading',{name:'连接你的 Kiro'});
  fireEvent.click(screen.getByRole('button',{name:'概览'}));
  await waitFor(()=>expect((screen.getByRole('button',{name:'启用连接'}) as HTMLButtonElement).disabled).toBe(false));
  expect(invoke.mock.calls.some(([,p])=>p.path==='/api/activate')).toBe(false);
 });
 it.each([63,0,null,undefined])('uses availableCredits %s without inventing total minus used',async(availableCredits)=>{
  setup(true);const original=invoke.getMockImplementation()!;
  invoke.mockImplementation((command,payload)=>payload.path==='/api/usage'?Promise.resolve({usage:{availableCredits,usageBreakdownList:[{dimensionType:'CREDIT',currentUsageWithPrecision:10,usageLimitWithPrecision:100}]}}):original(command,payload));
  render(<App/>);await login();await connect();fireEvent.click(screen.getByRole('button',{name:'刷新 ↻'}));await waitFor(()=>expect(document.querySelector('.pending-indicator')).toBeNull());fireEvent.click(screen.getByRole('button',{name:/概览/}));
  await waitFor(()=>expect(document.querySelector('.balance-value')?.textContent).toBe(availableCredits==null?'100':String(availableCredits)));
 });
});

 describe('verified balance without a session',()=>{
 it('preserves verified balance across pages and never reads session usage before connection',async()=>{
  setup(true);render(<App/>);await login();
  for(const name of ['设置','概览'])fireEvent.click(screen.getByRole('button',{name}));
  fireEvent.click(screen.getByRole('button',{name:'刷新 ↻'}));
  await waitFor(()=>expect(document.querySelector('.shell')?.getAttribute('aria-busy')).toBe('false'));
  expect(document.querySelector('.balance-value')?.textContent).toBe('100');
  expect(screen.getByText(/余额更新于/)).toBeTruthy();
  expect(screen.getByTitle(/预留不计入已用积分/)).toBeTruthy();
  expect(invoke.mock.calls.some(([,p])=>p.path==='/api/usage')).toBe(false);
 });
 it('retains verified balance after failed activation and page navigation',async()=>{
  setup(true);HTMLDialogElement.prototype.showModal=function(){this.open=true;};const original=invoke.getMockImplementation()!;
  invoke.mockImplementation((c,p)=>p.path==='/api/activate'?Promise.reject(supportError('SK-CONNECT-003','failed')):original(c,p));
  render(<App/>);await login();fireEvent.click(screen.getByRole('button',{name:'启用连接'}));fireEvent.submit(document.querySelector('dialog form')!);
  await screen.findByRole('heading',{name:'连接你的 Kiro'});
  fireEvent.click(screen.getByRole('button',{name:'概览'}));fireEvent.click(screen.getByRole('button',{name:'刷新 ↻'}));
  await waitFor(()=>expect(document.querySelector('.shell')?.getAttribute('aria-busy')).toBe('false'));
  fireEvent.click(screen.getByRole('button',{name:/概览/}));expect(document.querySelector('.balance-value')?.textContent).toBe('100');
  expect(invoke.mock.calls.some(([,p])=>p.path==='/api/usage')).toBe(false);
 });
 it('names the two known diagnostic checks without exposing unknown names',()=>{
  expect(sanitizeChecks([{name:'网关 TLS 代理路径',level:'warning'},{name:'真实 IDE 交互验收',level:'warning'},{name:'private-path',level:'fail'}]).map(c=>c.name)).toEqual(['网关 TLS 代理路径','真实 IDE 交互验收','检查项 3']);
 });
 });

it('does not derive a balance from legacy total minus used without a confirmed balance',async()=>{
 setup(true);const original=invoke.getMockImplementation()!;
 invoke.mockImplementation(async(c,p)=>{const result=await original(c,p);return p.path==='/api/status'?{...result,authenticated:true,has_snapshot:true}:result;});
 render(<App/>);await screen.findByRole('button',{name:'打开 Kiro ↗'});
 await waitFor(()=>expect(invoke.mock.calls.some(([,p])=>p.path==='/api/usage')).toBe(true));
 fireEvent.click(screen.getByRole('button',{name:'刷新 ↻'}));await waitFor(()=>expect(document.querySelector('.pending-indicator')).toBeNull());expect(screen.getByText('今日已用积分')).toBeTruthy();
 fireEvent.click(screen.getByRole('button',{name:/概览/}));expect(document.querySelector('.balance-value')?.textContent).toBe('—');
 expect(screen.queryByText(/余额更新于/)).toBeNull();
});

describe('persisted operation failure diagnostics',()=>{
 it('whitelists failure metadata and never includes raw host errors or paths',()=>{
  const summary=operationFailureSummary({state:'failed',stage:'apply',code:'permission',http_status:403,finished_at:1700000000,error:'secret',path:'private'});
  expect(summary).toContain('阶段 apply · 错误码 permission · HTTP 403');expect(summary).toContain('2023-11-14T22:13:20.000Z');
  expect(report([],'',{},summary)).toContain(summary);expect(summary).not.toMatch(/secret|private/);
  const invalid=operationFailureSummary({state:'failed',stage:'secret',code:'/private',http_status:999,finished_at:Infinity,error:'raw'});
  expect(invalid).toContain('阶段 unknown · 错误码 unknown');expect(invalid).not.toMatch(/secret|private|HTTP|raw/);
  expect(operationFailureSummary({state:'succeeded',stage:'apply',code:'permission'})).not.toContain('阶段 apply');
 });
 it('startup recovery has no diagnosis or report UI or requests',async()=>{
  setup(false,true);render(<App/>);await screen.findByRole('heading',{name:'本机配置待恢复'});
  expect(screen.getByRole('button',{name:'还原 Kiro 配置'})).toBeTruthy();
  expect(screen.queryByRole('button',{name:/诊断|报告/})).toBeNull();
  expect(invoke.mock.calls.some(([,p])=>p.path?.startsWith('/api/doctor'))).toBe(false);
 });
});

describe('confirmed balance session boundaries',()=>{
 it.each([25,undefined])('new card balance %s never inherits the previous card confirmed balance or timestamp',async(balance)=>{
  setup(true);HTMLDialogElement.prototype.showModal=function(){this.open=true;};const original=invoke.getMockImplementation()!;let second=false;
  invoke.mockImplementation(async(c,p)=>{
   if(p.path==='/api/verify-card')return {success:true,authorization:second?{remainingPoints:balance,totalPoints:300}:{remainingPoints:1234,totalPoints:2000}};
   const result=await original(c,p);
   // A disconnected host may still include stale authorization metadata.
   return p.path==='/api/status'?{...result,authorization:{remainingPoints:1234,totalPoints:2000}}:result;
  });
  render(<App/>);await login();expect(document.querySelector('.balance-value')?.textContent).toBe('1,234');
  fireEvent.click(screen.getByRole('button',{name:/设置/}));fireEvent.click(screen.getByRole('button',{name:/切换卡密/}));fireEvent.submit(document.querySelector('dialog form')!);
  await screen.findByRole('heading',{name:'卡密登录'});expect(screen.queryByText(/余额更新于/)).toBeNull();
  second=true;await login();expect(document.querySelector('.balance-value')?.textContent).toBe(balance===undefined?'—':'25');
  if(balance===undefined)expect(screen.queryByText(/余额更新于/)).toBeNull();
  expect(document.body.textContent).not.toContain('1,234');
 });
 it('restored exit clears balance and ignores an outstanding previous-session usage response',async()=>{
  setup(true);const original=invoke.getMockImplementation()!;let resolveUsage!:(v:unknown)=>void;
  invoke.mockImplementation((c,p)=>p.path==='/api/usage'?new Promise(resolve=>{resolveUsage=resolve;}):original(c,p));
  render(<App/>);await login();await connect();
  fireEvent.click(screen.getByRole('button',{name:/设置/}));fireEvent.click(screen.getByRole('button',{name:'退出程序'}));fireEvent.submit(document.querySelector('dialog form')!);
  await waitFor(()=>expect(invoke).toHaveBeenCalledWith('native',{method:'exit',args:[]}));
  resolveUsage({usage:{availableCredits:777,usageBreakdownList:[{dimensionType:'CREDIT',currentUsageWithPrecision:1,usageLimitWithPrecision:1000}]}});
  await screen.findByRole('heading',{name:'卡密登录'});expect(document.querySelector('.balance-value')).toBeNull();expect(screen.queryByText(/余额更新于/)).toBeNull();
  // Native exit is mocked, so verify another login cannot recover the late old balance.
  await login();expect(document.querySelector('.balance-value')?.textContent).toBe('100');expect(document.body.textContent).not.toContain('777');
 });
});

 describe('host heartbeat recovery',()=>{
 it('requires consecutive failures and clears connection warning after recovery',async()=>{
  vi.useFakeTimers();setup(true);let fail=true;const original=invoke.getMockImplementation()!;
  invoke.mockImplementation((command,payload)=>payload.path==='/api/heartbeat'?(fail?Promise.reject(supportError('SK-NET-002','failed')):Promise.resolve({status:'alive'})):original(command,payload));
  try{render(<App/>);await act(async()=>{await vi.advanceTimersByTimeAsync(15000);});expect(screen.queryByText('本地服务暂未响应，正在重试检测。')).toBeNull();
   await act(async()=>{await vi.advanceTimersByTimeAsync(15000);});expect(screen.getByText('本地服务暂未响应，正在重试检测。')).toBeTruthy();
   fail=false;await act(async()=>{await vi.advanceTimersByTimeAsync(15000);});expect(screen.queryByText('本地服务暂未响应，正在重试检测。')).toBeNull();
  }finally{vi.useRealTimers();}
 });
 });

 describe('restore keeps desktop authorization',()=>{
 it('restores with consent, retains balance, and permits reconnect without another login',async()=>{
  setup(true);render(<App/>);await login();await connect();
  fireEvent.click(screen.getByRole('button',{name:'还原 Kiro 配置'}));
  expect(document.querySelector('dialog[aria-labelledby=confirm-title]')?.textContent).toContain('不会被强制结束');
  fireEvent.submit(document.querySelector('dialog form')!);
  await screen.findByText('Kiro 配置已还原，当前卡密与积分信息已保留。');
  expect(screen.queryByRole('heading',{name:'卡密登录'})).toBeNull();
  expect(document.querySelector('.balance-value')?.textContent).toBe('100');
  expect(invoke).toHaveBeenCalledWith('api',{path:'/api/restore',method:'POST',body:{close_kiro_confirmed:true}});
  expect(invoke.mock.calls.some(([,p])=>p.method==='clear_remembered_card')).toBe(false);
  await connect();
  expect(invoke.mock.calls.filter(([,p])=>p.path==='/api/activate')).toHaveLength(2);
  expect(invoke.mock.calls.filter(([,p])=>p.path==='/api/verify-card')).toHaveLength(1);
 });
 it('cancel does not restore or sign out',async()=>{
  setup(true);render(<App/>);await login();await connect();
  fireEvent.click(screen.getByRole('button',{name:'还原 Kiro 配置'}));
  fireEvent.click(screen.getByRole('button',{name:'取消'}));
  expect(invoke.mock.calls.some(([,p])=>p.path==='/api/restore')).toBe(false);
  expect(screen.queryByRole('heading',{name:'卡密登录'})).toBeNull();
 });
 });


 it('ignores usage responses from the connection that was restored',async()=>{
  setup(true);const original=invoke.getMockImplementation()!;let resolveUsage!:(value:unknown)=>void;
  invoke.mockImplementation((c,p)=>p.path==='/api/usage'?new Promise(resolve=>{resolveUsage=resolve;}):original(c,p));
  render(<App/>);await login();await connect();
  fireEvent.click(screen.getByRole('button',{name:'还原 Kiro 配置'}));fireEvent.submit(document.querySelector('dialog form')!);
  await screen.findByText('Kiro 配置已还原，当前卡密与积分信息已保留。');
  await act(async()=>resolveUsage({usage:{availableCredits:777,usageBreakdownList:[{dimensionType:'CREDIT',currentUsageWithPrecision:1,usageLimitWithPrecision:1000}]}}));
  expect(document.querySelector('.balance-value')?.textContent).toBe('100');
  fireEvent.click(screen.getByRole('button',{name:/设置/}));fireEvent.click(screen.getByRole('button',{name:/概览/}));
  expect(document.querySelector('.balance-value')?.textContent).toBe('100');
 });


describe('desktop usability regressions',()=>{
 it('offers an explicit return to login from unauthenticated diagnosis',async()=>{
  setup(false,true);render(<App/>);
  await screen.findByRole('heading',{name:'本机配置待恢复'});
  fireEvent.click(screen.getByRole('button',{name:'返回卡密登录'}));
  expect(screen.getByRole('heading',{name:'卡密登录'})).toBeTruthy();
  expect(invoke.mock.calls.some(([,p])=>p.path==='/api/activate')).toBe(false);
 });
 it('information dialogs describe themselves, focus their only action, and restore focus',async()=>{
  setup(false);HTMLDialogElement.prototype.showModal=function(){this.open=true;};render(<App/>);
  const help=screen.getByRole('button',{name:'登录帮助 ↗'});help.focus();fireEvent.click(help);
  const dialog=screen.getByRole('dialog',{name:'登录帮助'});
  expect(dialog.getAttribute('aria-describedby')).toBe('confirm-detail');
  expect(document.getElementById('confirm-detail')?.textContent).toContain('不修改本机环境');
  expect(screen.queryByRole('button',{name:'取消'})).toBeNull();
  const accept=screen.getByRole('button',{name:'知道了'});expect(document.activeElement).toBe(accept);
  fireEvent.click(accept);expect(document.activeElement).toBe(help);
 });
 it('settings groups are explicitly selectable without losing account actions',async()=>{
  setup(false);render(<App/>);await login();
  fireEvent.click(screen.getByRole('button',{name:/设置/}));
  const selector=screen.getByLabelText('设置分组');
  for(const group of ['memory','account','install']){
   fireEvent.change(selector,{target:{value:group}});
   expect(document.querySelectorAll('.settings-group[data-active="true"]')).toHaveLength(1);
  }
  expect(screen.getByRole('button',{name:'退出程序'})).toBeTruthy();
  expect(screen.getByText('未检测到安装').className).toBe('muted');
 });
});


describe('session boundaries and credential cleanup feedback',()=>{
 it.each(['切换卡密','解除设备绑定'])('preserves cleanup failure after %s and allows retry from login',async action=>{
  setup(true);HTMLDialogElement.prototype.showModal=function(){this.open=true;};
  const original=invoke.getMockImplementation()!;let failClear=true;
  invoke.mockImplementation((command,payload)=>{
   if(command==='native'&&payload.method==='clear_remembered_card')return failClear?Promise.reject(supportError('SK-LOCAL-003','failed')):Promise.resolve(true);
   return original(command,payload);
  });
  render(<App/>);await login();fireEvent.click(screen.getByRole('button',{name:'设置'}));
  fireEvent.click(screen.getByRole('button',{name:action}));
  if(action==='解除设备绑定')fireEvent.change(screen.getByLabelText('当前卡密'),{target:{value:'secret-card'}});
  fireEvent.submit(document.querySelector('dialog form')!);
  await screen.findByRole('heading',{name:'卡密登录'});
  expect(screen.getByText('配置已还原，但系统保存的卡密未能清除，请在登录页取消记住卡密后重试。')).toBeTruthy();
  expect(screen.queryByText('Kiro 配置已还原。')).toBeNull();
  const remember=screen.getByLabelText('记住卡密') as HTMLInputElement;expect(remember.checked).toBe(true);
  failClear=false;fireEvent.click(remember);await waitFor(()=>expect(remember.checked).toBe(false));
  expect(screen.getByText('系统保存的卡密已清除。')).toBeTruthy();
  expect(screen.queryByText(/但系统保存的卡密未能清除/)).toBeNull();
 });
 it.each([false,true])('restricts unauthenticated navigation, including recovery=%s',async recovery=>{
  setup(true);const original=invoke.getMockImplementation()!;
  invoke.mockImplementation(async(command,payload)=>{
   const value=await original(command,payload);
   if(payload.path==='/api/status')return {...value,recovery_pending:recovery,has_snapshot:!recovery};
   if(payload.path?.startsWith('/api/doctor'))return {items:[{name:'Kiro Installation',level:'pass'}]};
   return value;
  });
  render(<App/>);
  await screen.findByRole('heading',{name:'本机配置待恢复'});
  for(const name of ['概览','用量','诊断','设置'])expect(screen.queryByRole('button',{name})).toBeNull();
  expect(screen.getByRole('button',{name:'还原 Kiro 配置'})).toBeTruthy();
  expect(screen.queryByRole('button',{name:/诊断|报告/})).toBeNull();
  expect(invoke.mock.calls.some(([,p])=>p.path==='/api/usage'||p.path==='/api/memory/sample')).toBe(false);
 });
 it('defers automatic announcements throughout guest recovery until a verified session exists',async()=>{
  setup(true,true);const original=invoke.getMockImplementation()!;
  invoke.mockImplementation((command,payload)=>payload.path==='/api/announcements'?Promise.resolve({announcements:[{id:'guest-gate',title:'通知',content:'会话公告',level:'info',created_at:1,expires_at:null}]}):original(command,payload));
  HTMLDialogElement.prototype.showModal=function(){this.open=true;};
  HTMLDialogElement.prototype.close=function(){this.open=false;};
  const focused=vi.spyOn(document,'hasFocus').mockReturnValue(true);
  vi.stubGlobal('ResizeObserver',class{observe(){} disconnect(){}});
  try{
   render(<App/>);await waitFor(()=>expect(invoke.mock.calls.some(([,p])=>p.path==='/api/announcements')).toBe(true));
   await screen.findByRole('heading',{name:'本机配置待恢复'});
   fireEvent.focus(window);
   expect(document.querySelector('.announcements-dialog[open]')).toBeNull();
   fireEvent.click(screen.getByRole('button',{name:'返回卡密登录'}));
   await login();
   expect(screen.getByRole('button',{name:'设置'})).toBeTruthy();
   await waitFor(()=>expect(document.querySelector('.announcements-dialog[open]')).toBeTruthy());
  }finally{focused.mockRestore();vi.unstubAllGlobals();}
 });
});

describe('updated desktop UI contracts',()=>{
 it('removes the login footer and opens the website through native',async()=>{
  setup();render(<App/>);await screen.findByRole('heading',{name:'卡密登录'});
  expect(document.querySelector('.login footer, .login-footer')).toBeNull();
  for(const name of ['无需卡密，查看诊断与恢复','获取卡密 ↗'])expect(screen.queryByRole('button',{name})).toBeNull();
  fireEvent.click(screen.getByRole('button',{name:'打开官网'}));
  await waitFor(()=>expect(invoke).toHaveBeenCalledWith('native',{method:'open_external',args:['https://kiro.rent/']}));
  expect(invoke.mock.calls.some(([,p])=>p.path==='/api/activate')).toBe(false);
 });
 it('renders balance and settled points with at most one decimal and no currency amounts',async()=>{
  setup(true);const original=invoke.getMockImplementation()!;
  invoke.mockImplementation(async(c,p)=>{
   if(p.path==='/api/verify-card')return {success:true,authorization:{remainingPoints:123.456,totalPoints:234.567}};
   if(p.path==='/api/usage')return {usage:{usageBreakdownList:[{dimensionType:'CREDIT',currentUsageWithPrecision:12.345,usageLimitWithPrecision:234.567}]},settledUsage:{todayPoints:3.456,referencePrice:999.999,daily:[{date:'2026-09-20',points:4.567,usd:888.888}],models:[{name:'Test model',points:5.678}]}};
   return original(c,p);
  });
  render(<App/>);await login();expect(document.querySelector('.balance-value')?.textContent).toBe('123.5');
  expect(screen.getByText('总积分 234.6')).toBeTruthy();await connect();
  fireEvent.click(screen.getByRole('button',{name:'刷新 ↻'}));await screen.findByText('3.5 积分');
  expect(screen.getByText('今日已用积分')).toBeTruthy();
  expect(screen.queryByText('Test model')).toBeNull();
  expect(document.body.textContent).not.toMatch(/USD|\$|999\.999|888\.888|12\.345|3\.456|4\.567|5\.678/);
 });
 it('expires every notice at five seconds while the error-code panel waits for the user',async()=>{
  setup();const original=invoke.getMockImplementation()!;
  invoke.mockImplementation((c,p)=>c==='native'&&p.method==='open_external'?Promise.reject(supportError('SK-NET-002','failed')):original(c,p));
  render(<App/>);await waitFor(()=>expect((screen.getByLabelText('记住卡密') as HTMLInputElement).disabled).toBe(false));
  vi.useFakeTimers();
  try{
   await act(async()=>{fireEvent.click(screen.getByLabelText('记住卡密'));});
   await act(async()=>{fireEvent.click(screen.getByLabelText('记住卡密'));});
   expect(screen.getByText('系统保存的卡密已清除。')).toBeTruthy();
   await act(async()=>{await vi.advanceTimersByTimeAsync(4999);});
   expect(screen.getByText('系统保存的卡密已清除。')).toBeTruthy();
   await act(async()=>{await vi.advanceTimersByTimeAsync(1);});
   expect(screen.queryByText('系统保存的卡密已清除。')).toBeNull();
   // A notice that happens to describe a failure is still a notice: it expires
   // on the same clock. The reportable record is the panel, and that one waits.
   await act(async()=>{fireEvent.click(screen.getByRole('button',{name:'打开官网'}));});
   expect(document.querySelector('.toast')).toBeTruthy();
   const panel=screen.getByRole('region',{name:'错误反馈'});
   expect(panel.textContent).toContain('错误码：SK-NET-002');
   await act(async()=>{await vi.advanceTimersByTimeAsync(5000);});
   expect(document.querySelector('.toast')).toBeNull();
   expect(screen.getByRole('region',{name:'错误反馈'}).textContent).toContain('错误码：SK-NET-002');
   expect(screen.getByRole('button',{name:'关闭错误反馈'})).toBeTruthy();
  }finally{vi.useRealTimers();}
 });
 it('restarts the clock when the identical notice repeats',async()=>{
  setup();const original=invoke.getMockImplementation()!;
  invoke.mockImplementation((c,p)=>c==='native'&&p.method==='open_external'?Promise.reject(supportError('SK-NET-002','failed')):original(c,p));
  render(<App/>);await waitFor(()=>expect((screen.getByLabelText('记住卡密') as HTMLInputElement).disabled).toBe(false));
  vi.useFakeTimers();
  try{
   await act(async()=>{fireEvent.click(screen.getByRole('button',{name:'打开官网'}));});
   const first=document.querySelector('.toast');expect(first).toBeTruthy();
   await act(async()=>{await vi.advanceTimersByTimeAsync(4000);});
   // Same failure, same sentence. React bails out on an identical string, so a
   // text-keyed timer would let the repeat inherit the first one's last second.
   await act(async()=>{fireEvent.click(screen.getByRole('button',{name:'打开官网'}));});
   await act(async()=>{await vi.advanceTimersByTimeAsync(4999);});
   expect(document.querySelector('.toast')).toBeTruthy();
   await act(async()=>{await vi.advanceTimersByTimeAsync(1);});
   expect(document.querySelector('.toast')).toBeNull();
  }finally{vi.useRealTimers();}
 });
 it('keeps an unread error panel visible while a later operation is still running',async()=>{
  setup();const original=invoke.getMockImplementation()!;
  invoke.mockImplementation((c,p)=>c==='native'&&p.method==='open_external'?Promise.reject(supportError('SK-NET-002','failed')):original(c,p));
  render(<App/>);await waitFor(()=>expect((screen.getByLabelText('记住卡密') as HTMLInputElement).disabled).toBe(false));
  fireEvent.click(screen.getByRole('button',{name:'打开官网'}));
  await screen.findByRole('region',{name:'错误反馈'});
  // Now start an operation that never settles. Blanking the panel when an
  // operation *begins* destroys an error the user has not read, and once the
  // notice expires on its own clock nothing is left to read.
  let settle:(v:unknown)=>void=()=>{};
  invoke.mockImplementation((c,p)=>c==='native'&&p.method==='open_external'?new Promise(r=>{settle=r;}):original(c,p));
  fireEvent.click(screen.getByRole('button',{name:'打开官网'}));
  await new Promise(r=>setTimeout(r,60));
  expect(screen.getByRole('region',{name:'错误反馈'}).textContent).toContain('错误码：SK-NET-002');
  // Only a clean success retires it.
  await act(async()=>{settle(true);await Promise.resolve();});
  await waitFor(()=>expect(screen.queryByRole('region',{name:'错误反馈'})).toBeNull());
 });
 it.each(['tray','minimize','exit'])('reads and saves the native close preference %s',async behavior=>{
  setup(true);const original=invoke.getMockImplementation()!;
  invoke.mockImplementation(async(c,p)=>c==='native'&&p.method==='get_close_behavior'?behavior:original(c,p));
  render(<App/>);await login();fireEvent.click(screen.getByRole('button',{name:'设置'}));
  const select=screen.getByLabelText('关闭窗口时') as HTMLSelectElement;
  await waitFor(()=>expect(select.value).toBe(behavior));
  expect(invoke).toHaveBeenCalledWith('native',{method:'get_close_behavior',args:[]});
  expect(Array.from(select.options,o=>o.value)).toEqual(['tray','minimize','exit']);
  const next=behavior==='tray'?'minimize':behavior==='minimize'?'exit':'tray';
  fireEvent.change(select,{target:{value:next}});
  await waitFor(()=>expect(invoke).toHaveBeenCalledWith('native',{method:'set_close_behavior',args:[next]}));
  await waitFor(()=>expect(select.value).toBe(next));
  expect(invoke.mock.calls.some(([,p])=>p.path==='/api/restore'||p.method==='exit')).toBe(false);
 });
});


describe('overview-only usage and navigation',()=>{
 it('exposes only overview/settings, hides token and memory stats, and refreshes status before settled usage',async()=>{
  setup(true);const original=invoke.getMockImplementation()!;let refreshed=false;
  invoke.mockImplementation(async(c,p)=>{
   if(p.path==='/api/usage')return {usage:{availableCredits:refreshed?88.888:100,usageBreakdownList:[{dimensionType:'CREDIT',currentUsageWithPrecision:99,usageLimitWithPrecision:200}]},settledUsage:{timezone:'UTC',todayPoints:refreshed?12.345:0,todayTokens:987654321,totalTokens:123456789}};
   const value=await original(c,p);
   return p.path==='/api/status'?{...value,authenticated:true,has_snapshot:true}:value;
  });
  render(<App/>);await screen.findByText('0 积分');
  const nav=within(screen.getByRole('navigation',{name:'主导航'}));
  expect(nav.getAllByRole('button')).toHaveLength(2);
  for(const name of ['概览','设置'])expect(nav.getByRole('button',{name})).toBeTruthy();
  for(const name of ['用量','诊断'])expect(nav.queryByRole('button',{name})).toBeNull();
  expect(screen.getByText('今日已用积分').getAttribute('title')).toBe('按 UTC 日期统计，仅含已结算请求');
  expect(screen.getByText('剩余积分')).toBeTruthy();
  expect(document.body.textContent).not.toMatch(/tokens|987654321|123456789|Kiro 当前占用/i);
  expect(document.querySelectorAll('.metrics > div')).toHaveLength(1);
  invoke.mockClear();refreshed=true;
  fireEvent.click(screen.getByRole('button',{name:'刷新 ↻'}));
  await screen.findByText('12.3 积分');
  expect(document.querySelector('.balance-value')?.textContent).toBe('88.9');
  const calls=invoke.mock.calls.map(([,p])=>p.path);
  expect(calls.filter(path=>path==='/api/usage')).toHaveLength(1);
  expect(calls.indexOf('/api/status')).toBeGreaterThanOrEqual(0);
  expect(calls.indexOf('/api/status')).toBeLessThan(calls.indexOf('/api/usage'));
  expect(calls).not.toContain('/api/activate');
  fireEvent.click(nav.getByRole('button',{name:'设置'}));
  expect(within(screen.getByRole('navigation')).getAllByRole('button')).toHaveLength(2);
 });
});


it('uses native total_process_count when Kiro exits during sampling',async()=>{
 setup(true);const original=invoke.getMockImplementation()!;
 invoke.mockImplementation((c,p)=>p.path==='/api/memory/sample'?Promise.resolve({total_memory_mb:0,total_process_count:0,ide_memory_mb:0,agent_memory_mb:0}):original(c,p));
 render(<App/>);await login();fireEvent.click(screen.getByRole('button',{name:'设置'}));
 await screen.findByText('Kiro 未运行');
 expect(screen.getByText('未运行')).toBeTruthy();
 expect((screen.getByRole('button',{name:'立即整理'}) as HTMLButtonElement).disabled).toBe(true);
});


describe('memory sampling invalidation',()=>{
 it.each([
  ['restore','status',false],['restore','memory',false],['restore','memory',true],
  ['switch','status',false],['switch','memory',false],['switch','memory',true],
 ] as const)('ignores delayed %s/%s response (reject=%s) after configuration or session changes',async(action,stage,rejectOld)=>{
  setup(true);
  const base=invoke.getMockImplementation()!;
  const fresh={total_memory_mb:222,total_process_count:2,ide_memory_mb:200,agent_memory_mb:22};
  invoke.mockImplementation((c,p)=>p.path==='/api/memory/sample'?Promise.resolve(fresh):base(c,p));
  render(<App/>);await login();await connect();
  await waitFor(()=>expect(document.querySelector('.pending-indicator')).toBeNull());
  const original=invoke.getMockImplementation()!;
  let resolveOld!:(value:unknown)=>void, failOld!:(reason:unknown)=>void;
  let delay=true;
  invoke.mockImplementation((c,p)=>{
   if(delay&&p.path===(stage==='status'?'/api/status':'/api/memory/sample')){
    delay=false;return new Promise((resolve,reject)=>{resolveOld=resolve;failOld=reject;});
   }
   return original(c,p);
  });
  fireEvent.click(screen.getByRole('button',{name:'设置'}));
  await waitFor(()=>expect(resolveOld).toBeTypeOf('function'));
  if(action==='restore'){
   fireEvent.click(screen.getByRole('button',{name:'概览'}));
   fireEvent.click(screen.getByRole('button',{name:'还原 Kiro 配置'}));
  }else fireEvent.click(screen.getByRole('button',{name:'切换卡密'}));
  fireEvent.submit(document.querySelector('dialog form')!);
  if(action==='restore')await screen.findByText('Kiro 配置已还原，当前卡密与积分信息已保留。');
  else {await screen.findByRole('heading',{name:'卡密登录'});await login();}
  fireEvent.click(screen.getByRole('button',{name:'设置'}));
  await waitFor(()=>expect(document.querySelector('.memory-ring-label strong')?.textContent).toBe('222'));
  const historyCount=document.querySelectorAll('.memory-trend i').length;
  await act(async()=>{
   if(rejectOld)failOld(supportError('SK-NET-002'));
   else resolveOld(stage==='status'?{kiro_installed:true,process_state:'Running',platform:'win32',authenticated:true,has_snapshot:true}:{total_memory_mb:999,total_process_count:1,ide_memory_mb:999,agent_memory_mb:0});
  });
  expect(document.querySelector('.memory-ring-label strong')?.textContent).toBe('222');
  expect(screen.queryByText('采样失败，当前占用与维护结果未确认')).toBeNull();
  expect(document.querySelectorAll('.memory-trend i')).toHaveLength(historyCount);
  fireEvent.click(screen.getByRole('button',{name:'概览'}));
  expect(screen.getByRole('button',{name:'启用连接'})).toBeTruthy();
  expect(screen.queryByRole('button',{name:'打开 Kiro ↗'})).toBeNull();
 });
});


describe('verification-only account operations',()=>{
 it.each(['切换卡密','解除设备绑定'])('%s does not claim to close an untouched IDE',async(action)=>{
  setup(true);HTMLDialogElement.prototype.showModal=function(){this.open=true;};
  render(<App/>);await login();
  fireEvent.click(screen.getByRole('button',{name:/设置/}));
  fireEvent.change(screen.getByLabelText('设置分组'),{target:{value:'account'}});
  fireEvent.click(screen.getByRole('button',{name:action}));
  expect(screen.getByRole('dialog').textContent).toContain('不会关闭 Kiro 或修改官方配置');
  if(action==='解除设备绑定')fireEvent.change(screen.getByLabelText('当前卡密'),{target:{value:'secret-card'}});
  fireEvent.submit(document.querySelector('dialog form')!);
  await screen.findByLabelText('输入你的卡密');
  expect(invoke.mock.calls.some(([,p])=>p.path===(action==='切换卡密'?'/api/restore':'/api/unbind'))).toBe(true);
  if(action==='解除设备绑定')expect(invoke.mock.calls.find(([,p])=>p.path==='/api/unbind')?.[1].body.gateway_url).toBe('https://example.com');
 });
});

 it('renders only safe auth categories and bounded retry hints',()=>{
  expect(safeError('[connection:authenticate] Authentication rejected: 403 - [auth:device-binding] remote-secret [retry-after:60]')).toBe('[connection:authenticate] 设备绑定不匹配，请先解除原设备绑定。 请在 60 秒后重试。');
  expect(safeError('[auth:locked-out] remote-secret [retry-after:86401]')).toBe('认证暂时锁定，请稍后重试。');
  expect(safeError('[auth:remote-secret] remote-secret')).not.toContain('remote-secret');
 });

it('keeps account actions compact and opens the official download section',async()=>{
 setup(true);const original=invoke.getMockImplementation()!;
 invoke.mockImplementation(async(c,p)=>{const result=await original(c,p);return p.path==='/api/status'?{...result,app_version:'0.1.0-preview.123456789abc'}:result;});
 HTMLDialogElement.prototype.showModal=function(){this.open=true;};
 render(<App/>);await login();fireEvent.click(screen.getByRole('button',{name:'设置'}));
 fireEvent.change(screen.getByLabelText('设置分组'),{target:{value:'account'}});
 expect(screen.queryByText(/升级前请保存工作/)).toBeNull();
 expect(document.querySelectorAll('.settings-actions > button')).toHaveLength(6);
 expect(document.querySelector('.settings-actions > p')).toBeNull();
 fireEvent.click(screen.getByRole('button',{name:'版本信息'}));
 expect(screen.getByRole('dialog').textContent).toContain('0.1.0-preview.123456789abc');
 fireEvent.submit(document.querySelector('dialog form')!);
 fireEvent.click(screen.getByRole('button',{name:'下载新版 ↗'}));
 await waitFor(()=>expect(invoke).toHaveBeenCalledWith('native',{method:'open_external',args:['https://kiro.rent/#downloads']}));
});
it('retains a restarted session gateway for unbind after restoration',async()=>{
 setup(true);const original=invoke.getMockImplementation()!;let restored=false;
 invoke.mockImplementation(async(c,p)=>{
  if(p.path==='/api/restore')restored=true;
  const result=await original(c,p);
  return p.path==='/api/status'?{...result,authenticated:!restored,has_snapshot:!restored,...(!restored?{gateway_url:'https://custom.example'}:{})}:result;
 });
 HTMLDialogElement.prototype.showModal=function(){this.open=true;};render(<App/>);
 await screen.findByRole('button',{name:'还原 Kiro 配置'});
 await waitFor(()=>expect(invoke.mock.calls.some(([,p])=>p.path==='/api/usage')).toBe(true));
 fireEvent.click(screen.getByRole('button',{name:'还原 Kiro 配置'}));fireEvent.submit(document.querySelector('dialog form')!);
 await screen.findByText('Kiro 配置已还原，当前卡密与积分信息已保留。');
 fireEvent.click(screen.getByRole('button',{name:'设置'}));fireEvent.change(screen.getByLabelText('设置分组'),{target:{value:'account'}});
 fireEvent.click(screen.getByRole('button',{name:'解除设备绑定'}));fireEvent.change(screen.getByLabelText('当前卡密'),{target:{value:'secret-card'}});fireEvent.submit(document.querySelector('dialog form')!);
 await waitFor(()=>expect(invoke.mock.calls.find(([,p])=>p.path==='/api/unbind')?.[1].body.gateway_url).toBe('https://custom.example'));
});

it('keeps recovery evidence and gives actionable support guidance after restore failure',async()=>{
 setup(true);const original=invoke.getMockImplementation()!;
 invoke.mockImplementation((c,p)=>p.path==='/api/restore'?Promise.reject(supportError('SK-RESTORE-001','failed')):original(c,p));
 HTMLDialogElement.prototype.showModal=function(){this.open=true;};render(<App/>);await login();
 fireEvent.click(screen.getByRole('button',{name:'设置'}));fireEvent.change(screen.getByLabelText('设置分组'),{target:{value:'account'}});
 fireEvent.click(screen.getByRole('button',{name:'切换卡密'}));fireEvent.submit(document.querySelector('dialog form')!);
 await screen.findByRole('heading',{name:'恢复未完成'});
 expect(screen.getByText(/保留本机快照/).textContent).toContain('不要公开上传');
 expect(screen.getByText(/通过官网支持渠道/).textContent).toContain('反馈编号');
 expect(screen.getByRole('button',{name:'重新恢复'})).toBeTruthy();
 expect(screen.queryByRole('button',{name:/诊断|报告/})).toBeNull();
});


describe('launch and restored usage contracts',()=>{
 it('blocks repeated launches and restore while the host launch result is unknown',async()=>{
  setup(true);const original=invoke.getMockImplementation()!;
  invoke.mockImplementation((c,p)=>p.path==='/api/launch'?Promise.reject(supportError('SK-NET-001','unknown')):original(c,p));
  render(<App/>);await login();await connect();
  fireEvent.click(screen.getByRole('button',{name:'打开 Kiro ↗'}));
  await screen.findAllByText(/操作结果未确认/);
  expect((screen.getByRole('button',{name:'打开 Kiro ↗'}) as HTMLButtonElement).disabled).toBe(true);
  fireEvent.click(screen.getByRole('button',{name:'还原 Kiro 配置'}));
  await screen.findByText(/上次写入结果未确认/);
  expect(screen.getByRole('heading',{name:'连接配置已应用'})).toBeTruthy();
  expect(screen.queryByRole('heading',{name:'连接诊断'})).toBeNull();
  expect(invoke.mock.calls.filter(([,p])=>p.path==='/api/launch')).toHaveLength(1);
  expect(invoke.mock.calls.some(([,p])=>p.path==='/api/restore')).toBe(false);
 });
 it.each([{},null])('does not report a successful launch without an explicit success result: %j',async result=>{
  setup(true);const original=invoke.getMockImplementation()!;
  invoke.mockImplementation((c,p)=>p.path==='/api/launch'?Promise.resolve(result):original(c,p));
  render(<App/>);await login();await connect();
  fireEvent.click(screen.getByRole('button',{name:'打开 Kiro ↗'}));
  await screen.findAllByText(/操作结果未确认/);
  expect(screen.queryByText('已发送 Kiro 启动请求。')).toBeNull();
  expect(screen.getByRole('region',{name:'错误反馈'}).textContent).toContain('SK-UNKNOWN-001');
  expect((screen.getByRole('button',{name:'打开 Kiro ↗'}) as HTMLButtonElement).disabled).toBe(true);
 });
 it('clears settled today usage after restore while retaining the confirmed balance',async()=>{
  setup(true);const original=invoke.getMockImplementation()!;
  invoke.mockImplementation((c,p)=>p.path==='/api/usage'?Promise.resolve({usage:{availableCredits:87,usageBreakdownList:[{dimensionType:'CREDIT',currentUsageWithPrecision:13,usageLimitWithPrecision:100}]},settledUsage:{timezone:'UTC',todayPoints:13}}):original(c,p));
  render(<App/>);await login();await connect();await screen.findByText('13 积分');
  fireEvent.click(screen.getByRole('button',{name:'还原 Kiro 配置'}));fireEvent.submit(document.querySelector('dialog form')!);
  await screen.findByText('Kiro 配置已还原，当前卡密与积分信息已保留。');
  expect(screen.queryByText('13 积分')).toBeNull();expect(screen.getByText('— 积分')).toBeTruthy();
  expect(document.querySelector('.balance-value')?.textContent).toBe('87');
  fireEvent.click(screen.getByRole('button',{name:'刷新 ↻'}));
  await waitFor(()=>expect((screen.getByRole('button',{name:'刷新 ↻'}) as HTMLButtonElement).disabled).toBe(false));
  expect(screen.queryByText('13 积分')).toBeNull();
 });
});

it('explains rebind policy errors without exposing remote details',()=>{
  expect(safeError('[auth:rebind-cooldown] remote-secret [retry-after:123]')).toBe('换绑仍在冷却期，请等待冷却结束后重试。 请在 123 秒后重试。');
  expect(safeError('[auth:rebind-limit] remote-secret')).toBe('换绑次数已用尽，请联系支持方；稍后重试不会恢复次数。');
});




describe('structured error feedback',()=>{
 const failure=()=>errors.toClientError({...supportError('SK-AUTH-003'),feedback_id:'feedback-test-123',message:'private upstream credential'});
 it.each([false,true])('shows safe login errors and copy feedback (clipboard fails=%s)',async fails=>{
  setup();const original=invoke.getMockImplementation()!;const error=failure();
  const normalize=vi.spyOn(errors,'toClientError').mockReturnValue(error);
  const feedback=vi.spyOn(errors,'feedbackText').mockReturnValue('safe support feedback');
  const writeText=vi.fn().mockImplementation(()=>fails?Promise.reject(new Error('clipboard secret')):Promise.resolve());
  vi.stubGlobal('navigator',Object.assign(Object.create(navigator),{clipboard:{writeText}}));
  invoke.mockImplementation((c,p)=>p.path==='/api/verify-card'?Promise.reject(error):original(c,p));
  try{
   render(<App/>);await waitFor(()=>expect((screen.getByLabelText('记住卡密') as HTMLInputElement).disabled).toBe(false));fireEvent.change(screen.getByLabelText('输入你的卡密'),{target:{value:'secret-card'}});fireEvent.click(screen.getByRole('button',{name:'登录 →'}));
   const card=await screen.findByRole('region',{name:'错误反馈'});
   expect(card.textContent).toContain(error.code);expect(card.textContent).toContain(error.feedback_id);
   expect(document.body.textContent).not.toMatch(/private upstream|secret-card/);
   fireEvent.click(within(card).getByRole('button',{name:'复制反馈信息'}));
   await screen.findByText(fails?'复制失败，请记录错误码和反馈编号，通过官网联系支持。':'已复制反馈信息。');
   expect(feedback).toHaveBeenCalledWith(error,expect.objectContaining({platform:'win32'}));
   expect(writeText).toHaveBeenCalledWith('safe support feedback');
   expect(screen.getByRole('heading',{name:'卡密登录'})).toBeTruthy();
   expect(card.textContent).toContain(error.feedback_id);
  }finally{normalize.mockRestore();feedback.mockRestore();vi.unstubAllGlobals();}
 });
 it.each(['failed','partial'] as const)('captures %s native failures without leaving settings',async outcome=>{
  setup(true);const original=invoke.getMockImplementation()!;const error=errors.toClientError(supportError(outcome==='partial'?'SK-BIND-004':'SK-LOCAL-001',outcome));
  const normalize=vi.spyOn(errors,'toClientError').mockReturnValue(error);
  invoke.mockImplementation((c,p)=>c==='native'&&p.method==='set_close_behavior'?Promise.reject(error):original(c,p));
  try{
   render(<App/>);await login();fireEvent.click(screen.getByRole('button',{name:'设置'}));
   fireEvent.change(screen.getByLabelText('设置分组'),{target:{value:'account'}});
   fireEvent.change(screen.getByLabelText('关闭窗口时'),{target:{value:'exit'}});
   expect((await screen.findByRole('region',{name:'错误反馈'})).textContent).toContain(error.feedback_id);
   expect(screen.getByRole('heading',{name:'设置'})).toBeTruthy();
   const card=screen.getByRole('region',{name:'错误反馈'});expect(card.textContent).toContain(error.message);
   expect(within(card).getByRole('heading',{name:outcome==='partial'?'云端操作已完成，本机仍需处理':'操作未完成'})).toBeTruthy();
   expect((screen.getByLabelText('关闭窗口时') as HTMLSelectElement).value).toBe('tray');
  }finally{normalize.mockRestore();}
 });
 it('keeps unknown restore results on settings and blocks switch and exit without navigation',async()=>{
  setup(true);const original=invoke.getMockImplementation()!;
  const error=Object.assign(failure(),{outcome:'unknown' as const});
  const normalize=vi.spyOn(errors,'toClientError').mockReturnValue(error);
  invoke.mockImplementation((c,p)=>p.path==='/api/restore'?Promise.reject(error):original(c,p));
  try{
   render(<App/>);await login();await connect();fireEvent.click(screen.getByRole('button',{name:'设置'}));
   fireEvent.change(screen.getByLabelText('设置分组'),{target:{value:'account'}});
   fireEvent.click(screen.getByRole('button',{name:'切换卡密'}));fireEvent.submit(document.querySelector('dialog form')!);
   await screen.findByRole('region',{name:'错误反馈'});
   await waitFor(()=>expect((screen.getByRole('button',{name:'切换卡密'}) as HTMLButtonElement).disabled).toBe(false));
   expect(screen.getByRole('heading',{name:'操作结果待确认'})).toBeTruthy();
   fireEvent.click(screen.getByRole('button',{name:'关闭错误反馈'}));
   expect(screen.queryByRole('region',{name:'错误反馈'})).toBeNull();
   for(const action of ['切换卡密','退出程序']){
    fireEvent.click(screen.getByRole('button',{name:action}));
    expect(screen.getByRole('heading',{name:'设置'})).toBeTruthy();expect(document.querySelector('dialog form')).toBeNull();
   }
   expect(invoke.mock.calls.filter(([,p])=>p.path==='/api/restore')).toHaveLength(1);
   expect(invoke.mock.calls.some(([c,p])=>c==='native'&&p.method==='exit')).toBe(false);
   expect(screen.queryByRole('heading',{name:'恢复未完成'})).toBeNull();
  }finally{normalize.mockRestore();}
 });
});

describe('a Kiro that will not close',()=>{
 // The first request for `path` fails as `code`; after a later success the machine reads as restored.
 function kiroStaysOpenOnce(path:string,code='SK-CONNECT-005'){const original=invoke.getMockImplementation()!;let refused=false,done=false;invoke.mockImplementation(async(c,p)=>{if(p.path===path){if(!refused){refused=true;throw supportError(code);}done=true;return {success:true};}const result=await original(c,p);return p.path==='/api/status'&&done?{...result,authenticated:false,has_snapshot:false,recovery_pending:false}:result;});}
 const guidance=()=>document.querySelector('.page-restore-failed .subtitle')?.textContent??'';
 async function openAccount(action:string){fireEvent.click(screen.getByRole('button',{name:/设置/}));fireEvent.change(screen.getByLabelText('设置分组'),{target:{value:'account'}});fireEvent.click(screen.getByRole('button',{name:action}));}
 it('forces only after a second consent, replaying the unbind with the card already given',async()=>{
  setup(true);render(<App/>);await login();await connect();kiroStaysOpenOnce('/api/unbind');
  await openAccount('解除设备绑定');
  fireEvent.change(screen.getByLabelText('当前卡密'),{target:{value:'secret-card'}});fireEvent.submit(document.querySelector('dialog form')!);
  await screen.findByRole('heading',{name:'恢复未完成'});
  expect(guidance()).toContain('文件 > 退出');
  expect(invoke.mock.calls.find(([,p])=>p.path==='/api/unbind')?.[1].body.force_close_confirmed).toBeUndefined();
  fireEvent.click(screen.getByRole('button',{name:'强制关闭 Kiro 并继续'}));
  expect(screen.getByRole('dialog').textContent).toContain('未保存的内容将丢失');
  expect(screen.queryByLabelText('当前卡密')).toBeNull();
  fireEvent.submit(document.querySelector('dialog form')!);
  await screen.findByLabelText('输入你的卡密');
  const unbinds=invoke.mock.calls.filter(([,p])=>p.path==='/api/unbind');
  expect(unbinds).toHaveLength(2);
  expect(unbinds[1][1].body).toMatchObject({card_key:'secret-card',close_kiro_confirmed:true,force_close_confirmed:true});
  expect(invoke.mock.calls.some(([,p])=>p.path==='/api/restore')).toBe(false);
 });
 it('retries the intent that failed, not a plain restore',async()=>{
  setup(true);render(<App/>);await login();await connect();kiroStaysOpenOnce('/api/restore');
  await openAccount('切换卡密');fireEvent.submit(document.querySelector('dialog form')!);
  await screen.findByRole('heading',{name:'恢复未完成'});
  fireEvent.click(screen.getByRole('button',{name:'重新恢复'}));
  await screen.findByLabelText('输入你的卡密');
  const restores=invoke.mock.calls.filter(([,p])=>p.path==='/api/restore');
  expect(restores).toHaveLength(2);
  expect(restores.every(([,p])=>p.body.force_close_confirmed===undefined)).toBe(true);
  expect(invoke.mock.calls.some(([,p])=>p.method==='clear_remembered_card')).toBe(true);
 });
 it('a Kiro open in another session gets its own guidance and no force',async()=>{
  setup(true);render(<App/>);await login();await connect();kiroStaysOpenOnce('/api/restore','SK-CONNECT-006');
  fireEvent.click(screen.getByRole('button',{name:'还原 Kiro 配置'}));fireEvent.submit(document.querySelector('dialog form')!);
  await screen.findByRole('heading',{name:'恢复未完成'});
  expect(guidance()).toContain('另一个 Windows 会话');
  expect(screen.queryByRole('button',{name:'强制关闭 Kiro 并继续'})).toBeNull();
 });
 it.each([['unknown',true],['failed',false]] as const)('an operation left %s before a restart locks to local restore: %s',async(outcome,locked)=>{
  setup(true);const original=invoke.getMockImplementation()!;
  invoke.mockImplementation((c,p)=>p.path==='/api/operation'?Promise.resolve({id:7,state:'failed',support_error:supportError('SK-NET-001',outcome)}):original(c,p));
  render(<App/>);await screen.findByLabelText('输入你的卡密');
  if(locked)await screen.findByText(/上次操作的结果未确认/);
  await waitFor(()=>expect(invoke.mock.calls.some(([,p])=>p.path==='/api/operation')).toBe(true));
  expect(!!screen.queryByRole('button',{name:'仅还原本机配置'})).toBe(locked);
 });
});

describe('host lock and failed restore guidance',()=>{
 it('a restore the host refused before starting says so and leaves the page as it was',async()=>{setup(true,true);HTMLDialogElement.prototype.showModal=function(){this.open=true;};const original=invoke.getMockImplementation()!;invoke.mockImplementation(async(command,payload)=>payload.path==='/api/restore'?Promise.reject(supportError('SK-LOCAL-002','unknown')):original(command,payload));render(<App/>);await screen.findByRole('heading',{name:'本机配置待恢复'});fireEvent.click(screen.getByRole('button',{name:'还原 Kiro 配置'}));fireEvent.submit(document.querySelector('dialog form')!);await screen.findByText(/另一个操作正在进行，本次请求未执行/);expect(screen.queryByRole('heading',{name:'恢复未完成'})).toBeNull();expect(screen.queryByRole('heading',{name:'操作结果待确认'})).toBeNull();expect(screen.getByRole('heading',{name:'本机配置待恢复'})).toBeTruthy();expect((screen.getByRole('button',{name:'还原 Kiro 配置'}) as HTMLButtonElement).disabled).toBe(false);});
 it('a restore that needs a Kiro reinstall says so, offers the download and a way back',async()=>{setup(true,true);HTMLDialogElement.prototype.showModal=function(){this.open=true;};const original=invoke.getMockImplementation()!;invoke.mockImplementation(async(command,payload)=>{if(payload.path==='/api/restore')throw supportError('SK-RESTORE-002');const result=await original(command,payload);return payload.path==='/api/status'?{...result,recovery_blocked:'reinstall_kiro'}:result;});render(<App/>);await screen.findByRole('heading',{name:'本机配置待恢复'});fireEvent.click(screen.getByRole('button',{name:'还原 Kiro 配置'}));fireEvent.submit(document.querySelector('dialog form')!);await screen.findByRole('heading',{name:'恢复未完成'});const page=document.querySelector('main')!;expect(page.textContent).toContain('重新安装 Kiro');expect(page.textContent).not.toContain('暂停升级');expect(screen.queryByRole('navigation')).toBeNull();fireEvent.click(screen.getByRole('button',{name:'下载 Kiro ↗'}));await waitFor(()=>expect(invoke).toHaveBeenCalledWith('native',{method:'open_external',args:['https://kiro.dev/downloads/']}));fireEvent.click(screen.getByRole('button',{name:'返回概览'}));await screen.findByRole('heading',{name:'本机配置待恢复'});});
 it('an ordinary restore failure no longer forbids reinstalling and still has a way back',async()=>{setup(true,true);HTMLDialogElement.prototype.showModal=function(){this.open=true;};const original=invoke.getMockImplementation()!;invoke.mockImplementation(async(command,payload)=>payload.path==='/api/restore'?Promise.reject(supportError('SK-RESTORE-001')):original(command,payload));render(<App/>);await screen.findByRole('heading',{name:'本机配置待恢复'});fireEvent.click(screen.getByRole('button',{name:'还原 Kiro 配置'}));fireEvent.submit(document.querySelector('dialog form')!);await screen.findByRole('heading',{name:'恢复未完成'});expect(document.querySelector('main')!.textContent).not.toContain('暂停升级');expect(screen.queryByRole('button',{name:'下载 Kiro ↗'})).toBeNull();expect(screen.getByRole('button',{name:'重新恢复'})).toBeTruthy();fireEvent.click(screen.getByRole('button',{name:'返回概览'}));await screen.findByRole('heading',{name:'本机配置待恢复'});});
 it('an activation confirmed only by reconciliation loads the live balance',async()=>{setup(true);HTMLDialogElement.prototype.showModal=function(){this.open=true;};const original=invoke.getMockImplementation()!;let activated=false,confirmed=false;invoke.mockImplementation(async(command,payload)=>{if(payload.path==='/api/activate'){activated=true;throw supportError('SK-NET-001','unknown');}if(payload.path==='/api/operation'){if(!activated)return {id:0,state:'idle'};confirmed=true;return {id:1,state:'succeeded'};}const result=await original(command,payload);return payload.path==='/api/status'&&confirmed?{...result,authenticated:true,has_snapshot:true}:result;});render(<App/>);await login();fireEvent.click(screen.getByRole('button',{name:'启用连接'}));fireEvent.submit(document.querySelector('dialog form')!);await waitFor(()=>expect(invoke.mock.calls.some(([,p])=>p.path==='/api/usage')).toBe(true),{timeout:5000});await screen.findByRole('heading',{name:'连接配置已应用'});});
});

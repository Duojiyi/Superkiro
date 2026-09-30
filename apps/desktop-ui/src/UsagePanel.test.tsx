// @vitest-environment jsdom
import { afterEach, expect, it, vi } from 'vitest';
import { cleanup, fireEvent, render, screen } from '@testing-library/react';
import { UsagePanel } from './UsagePanel';
afterEach(cleanup);
it('shows points only and switches daily model consumption',()=>{
  const refresh=vi.fn();
  render(<UsagePanel state="ready" refresh={refresh} usage={{settledUsage:{activatedAt:86400,totalPoints:4,todayPoints:1,daily:[{date:'1970-01-02',points:4}],models:[{name:'model-a',points:3,daily:[{date:'1970-01-02',points:3}]},{name:'model-b',points:1,daily:[]}]}}}/>);
  expect(screen.getByText('累计消耗')).toBeTruthy();
  fireEvent.change(screen.getByLabelText('每日消耗模型'),{target:{value:'model-b'}});
  expect(screen.getByText('0')).toBeTruthy();
  expect(document.body.textContent).not.toMatch(/token|usd|美元/i);
  fireEvent.click(screen.getByLabelText('刷新用量'));
  expect(refresh).toHaveBeenCalledOnce();
});
it('does not label an old gateway thirty-day total as lifetime consumption',()=>{
  render(<UsagePanel state="ready" refresh={()=>{}} usage={{settledUsage:{todayPoints:1,daily:[]}}}/>);
  expect(screen.getByRole('status').textContent).toContain('完整用量暂不可用');
  expect(screen.queryByText('累计消耗')).toBeNull();
});
it('hides stale statistics after an error',()=>{
  render(<UsagePanel state="error" refresh={()=>{}} usage={{settledUsage:{activatedAt:1,totalPoints:123}}}/>);
  expect(screen.getByRole('status').textContent).toContain('读取失败');
  expect(screen.queryByText('123')).toBeNull();
});
it('charts UTC dates chronologically, filters models and pages back to activation',()=>{
 const daily=Array.from({length:16},(_,i)=>({date:`2026-09-${String(i+1).padStart(2,'0')}`,points:i}));
 render(<UsagePanel state="ready" refresh={()=>{}} usage={{settledUsage:{activatedAt:1788220800,totalPoints:120,todayPoints:15,daily,models:[{name:'model-a',points:120,daily}]}}}/>);
 expect(screen.getByText(/按 UTC 日期结算/)).toBeTruthy();
 const bars=()=>[...screen.getByRole('img',{name:'每日积分消耗趋势'}).querySelectorAll('rect')];
 expect(bars()).toHaveLength(14);
 expect(bars()[0].getAttribute('aria-label')).toBe('2026-09-03：2 积分');
 expect(bars()[13].getAttribute('aria-label')).toBe('2026-09-16：15 积分');
 fireEvent.click(screen.getByRole('button',{name:'较早日期'}));
 expect(bars()).toHaveLength(2);
 expect(bars()[0].getAttribute('aria-label')).toBe('2026-09-01：0 积分');
 fireEvent.change(screen.getByLabelText('每日消耗模型'),{target:{value:'model-a'}});
 expect(bars()).toHaveLength(14);
 expect(document.body.innerHTML).not.toMatch(/NaN|Infinity/);
});
it('does not draw missing model daily values as zero',()=>{
 render(<UsagePanel state="ready" refresh={()=>{}} usage={{settledUsage:{activatedAt:1,totalPoints:4,daily:[{date:'2026-09-01',points:4}],models:[{name:'missing',points:4}]}}}/>);
 fireEvent.change(screen.getByLabelText('每日消耗模型'),{target:{value:'missing'}});
 expect(screen.queryByRole('img')).toBeNull();
 expect(screen.getByText(/每日明细尚不完整/)).toBeTruthy();
});

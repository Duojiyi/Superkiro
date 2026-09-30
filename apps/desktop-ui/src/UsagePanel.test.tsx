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

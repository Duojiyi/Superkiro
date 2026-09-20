// @vitest-environment jsdom
import { afterEach, expect, it } from 'vitest';
import { cleanup, render, screen } from '@testing-library/react';
import { MemoryVisual } from './MemoryVisual';
afterEach(cleanup);
it('uses explicit stopped state, not a dash or a fabricated zero',()=>{
 render(<MemoryVisual state="empty" sample={null} samples={[]}/>);
 expect(screen.getByText('未运行')).toBeTruthy();
 expect(document.body.textContent).not.toContain('—');
 expect(document.querySelector('.memory-ring-app')).toBeNull();
});
it('renders measured process composition with truthful denominator',()=>{
 render(<MemoryVisual state="ready" sample={{total_memory_mb:1000,ide_memory_mb:750,agent_memory_mb:250,total_process_count:4}} samples={[900,1000]}/>);
 expect(screen.getByText('750 MB')).toBeTruthy();
 expect(screen.getByText('250 MB')).toBeTruthy();
 expect(screen.getByText('4 个进程 · 非整机内存占比')).toBeTruthy();
 expect(Number(document.querySelector('.memory-ring-app')!.getAttribute('stroke-dasharray')!.split(' ')[0])).toBeCloseTo(314.159*.75);
});
it('does not invent classification when older data lacks it',()=>{
 render(<MemoryVisual state="ready" sample={{total_memory_mb:1000,total_process_count:3}} samples={[1000]}/>);
 expect(screen.getAllByText('暂不可用')).toHaveLength(2);
 expect(document.querySelector('.memory-ring-app')).toBeNull();
});
it('does not show stale values on errors',()=>{
 render(<MemoryVisual state="error" sample={{total_memory_mb:999}} samples={[999]}/>);
 expect(document.body.textContent).not.toContain('999');
 expect(screen.getByText('采样失败')).toBeTruthy();
});
